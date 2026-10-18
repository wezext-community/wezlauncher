// ============================================================
//  wezlauncher —— 替代 wezterm 内置启动器的 TUI（Ctrl+Shift+O）
//
//  它是被 wezterm 用一个新标签页跑起来的（见 .wezterm.lua 的绑定），
//  通过 `wezterm cli` 读标签页列表、切标签、开新标签，最后把自己的标签关掉。
//
//  Tab 切页：标签页 / 新建
// ============================================================
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

fn find_wezterm() -> PathBuf {
    // 注意：在 wezterm-gui 里运行时，WEZTERM_EXECUTABLE 指向的是 wezterm-gui.exe，
    // 而 `cli` 子命令在 wezterm.exe 里 —— 所以先找它同目录的 wezterm.exe。
    if let Ok(p) = std::env::var("WEZTERM_EXECUTABLE") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            let name = pb
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if name == "wezterm.exe" {
                return pb;
            }
            if let Some(dir) = pb.parent() {
                let cand = dir.join("wezterm.exe");
                if cand.exists() {
                    return cand;
                }
            }
        }
    }
    for guess in [
        r"D:\Program Files\WezTerm\wezterm.exe",
        r"C:\Program Files\WezTerm\wezterm.exe",
    ] {
        let pb = PathBuf::from(guess);
        if pb.exists() {
            return pb;
        }
    }
    PathBuf::from("wezterm.exe")
}

fn cli(args: &[&str]) -> io::Result<String> {
    let out = Command::new(find_wezterm()).arg("cli").args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 自己的 pane id（wezterm 会注入 WEZTERM_PANE）
fn own_pane() -> Option<u64> {
    std::env::var("WEZTERM_PANE").ok()?.parse().ok()
}

#[derive(Clone)]
struct TabInfo {
    tab_id: u64,
    window_id: u64,
    index: usize,
    title: String,
    cwd: String,
    active: bool,
    panes: usize,
    zoomed: bool,
}

fn basename(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if last.is_empty() || last.len() == 2 {
        // 盘符根目录，比如 "I:"
        trimmed.to_string()
    } else {
        last.to_string()
    }
}

fn load_tabs() -> io::Result<Vec<TabInfo>> {
    let raw = cli(&["list", "--format", "json"])?;
    let json: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| io::Error::other(format!("解析 cli list 输出失败: {e}")))?;
    let Some(panes) = json.as_array() else {
        return Ok(Vec::new());
    };

    let own = own_pane();
    // 自己是哪个 tab / 哪个窗口：自己的 tab 不显示，
    // 别的窗口的 tab 也不显示（cli list 会把所有窗口都列出来）
    let own_entry = own.and_then(|pid| {
        panes
            .iter()
            .find(|p| p["pane_id"].as_u64() == Some(pid))
    });
    let own_tab = own_entry.and_then(|p| p["tab_id"].as_u64());
    let own_window = own_entry.and_then(|p| p["window_id"].as_u64());

    let mut tabs: Vec<TabInfo> = Vec::new();
    for pane in panes {
        let Some(tab_id) = pane["tab_id"].as_u64() else {
            continue;
        };
        if Some(tab_id) == own_tab {
            continue;
        }
        let window_id = pane["window_id"].as_u64().unwrap_or(0);
        if let Some(w) = own_window {
            if window_id != w {
                continue; // 只列本窗口的标签页
            }
        }
        let title = {
            let tt = pane["tab_title"].as_str().unwrap_or("");
            let raw = if tt.is_empty() {
                pane["title"].as_str().unwrap_or("").to_string()
            } else {
                tt.to_string()
            };
            // 有的 shell 把整条 exe 路径当标题，这里只留文件名
            let bs = char::from(92u8); // 反斜杠，用码点避免转义写错
            if raw.contains('/') || raw.contains(bs) {
                basename(&raw.replace(bs, "/"))
            } else {
                raw
            }
        };
        let cwd = pane["cwd"].as_str().unwrap_or("").to_string();
        let active = pane["is_active"].as_bool().unwrap_or(false);
        let zoomed = pane["is_zoomed"].as_bool().unwrap_or(false);
        match tabs.iter_mut().find(|t| t.tab_id == tab_id) {
            Some(t) => {
                t.panes += 1;
                t.active |= active;
                if active {
                    t.title = title;
                    t.cwd = cwd;
                }
            }
            None => tabs.push(TabInfo {
                tab_id,
                window_id,
                index: tabs.len(),
                title,
                cwd,
                active,
                panes: 1,
                zoomed,
            }),
        }
    }
    // 同一个窗口内按 tab_id 排序（tab_id 递增）
    tabs.sort_by_key(|t| (t.window_id, t.tab_id));
    for (i, t) in tabs.iter_mut().enumerate() {
        t.index = i;
    }
    Ok(tabs)
}

#[derive(Clone)]
struct NewItem {
    label: String,
    hint: String,
    /// spawn 参数（不含 wezterm cli 前缀）
    args: Vec<String>,
}

/// `wsl.exe -l -q` 输出是 UTF-16LE，需要自己解码
fn list_wsl() -> Vec<String> {
    let Ok(out) = Command::new("wsl.exe").args(["-l", "-q"]).output() else {
        return Vec::new();
    };
    let bytes = out.stdout;
    // UTF-16LE → String，丢掉 BOM
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    let text = text.trim_start_matches('\u{feff}');
    text.lines()
        .map(|l| l.trim().trim_end_matches('\0').to_string())
        .filter(|l| !l.is_empty())
        // docker-desktop 这类不是可用的 shell，跳过
        .filter(|l| !l.to_lowercase().starts_with("docker-desktop"))
        .collect()
}

fn ssh_config_path() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".wezterm-ssh.txt")
}

/// 自定义 SSH 条目：每行 `名字 = ssh 参数`，`#` 开头是注释
fn list_ssh() -> Vec<(String, Vec<String>)> {
    let Ok(text) = fs::read_to_string(ssh_config_path()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((label, rest)) = line.split_once('=') else {
            continue;
        };
        let label = label.trim();
        let args: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
        if !label.is_empty() && !args.is_empty() {
            out.push((label.to_string(), args));
        }
    }
    out
}

/// 写一份带注释的示例文件（仅在文件不存在时）
fn ensure_ssh_example() {
    let path = ssh_config_path();
    if path.exists() {
        return;
    }
    let body = [
        "# wezlauncher 的 SSH 快捷项",
        "# 每行： 名字 = ssh 参数",
        "# 例：",
        "#   家里 = 192.168.1.10",
        "#   生产 = deploy@example.com -p 2222",
        "#   跳板 = -i C:/Users/10678/.ssh/id_ed25519 user@10.0.0.5",
        "",
    ]
    .join("\n");
    let _ = fs::write(path, body);
}

/// 固定项 + 自动扫描到的 WSL 发行版 + 自定义 SSH
fn new_items() -> Vec<NewItem> {
    let mk = |label: &str, hint: &str, args: &[&str]| NewItem {
        label: label.to_string(),
        hint: hint.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    };

    let mut items = vec![
        mk(
            "PowerShell 7 标签页",
            "Windows",
            &["--", "pwsh.exe", "-NoLogo"],
        ),
        mk("CMD 标签页", "Windows", &["--", "cmd.exe"]),
        mk(
            "Git Bash 标签页",
            "Windows",
            &["--", "C:/Program Files/Git/bin/bash.exe", "-li"],
        ),
    ];

    // 所有已安装的 WSL 发行版（运行时枚举）
    for distro in list_wsl() {
        items.push(NewItem {
            label: format!("WSL: {distro}"),
            hint: "WSL 家目录".to_string(),
            args: vec![
                "--".into(),
                "wsl.exe".into(),
                "-d".into(),
                distro.clone(),
                "--cd".into(),
                "~".into(),
            ],
        });
    }

    // 自定义 SSH 条目
    for (label, args) in list_ssh() {
        let mut spawn_args = vec!["--".to_string(), "ssh.exe".to_string()];
        spawn_args.extend(args.iter().cloned());
        items.push(NewItem {
            label: format!("SSH: {label}"),
            hint: args.join(" "),
            args: spawn_args,
        });
    }

    items.push(mk(
        "新窗口（PowerShell）",
        "独立窗口",
        &["--new-window", "--", "pwsh.exe", "-NoLogo"],
    ));

    items
}

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Tabs,
    New,
}

struct App {
    tabs: Vec<TabInfo>,
    new_items: Vec<NewItem>,
    sel_tab: usize,
    sel_new: usize,
    page: Page,
    status: String,
    quit: bool,
}

impl App {
    fn new(start_on_new: bool) -> Self {
        let (tabs, status) = match load_tabs() {
            Ok(t) => {
                let s = format!("{} 个标签页（Esc 取消）", t.len());
                (t, s)
            }
            Err(e) => (Vec::new(), format!("读取标签页失败: {e}")),
        };
        App {
            tabs,
            new_items: new_items(),
            sel_tab: 0,
            sel_new: 0,
            page: if start_on_new { Page::New } else { Page::Tabs },
            status,
            quit: false,
        }
    }

    fn reload(&mut self) {
        self.new_items = new_items();
        match load_tabs() {
            Ok(t) => {
                self.tabs = t;
                self.status = format!("{} 个标签页", self.tabs.len());
                if self.sel_tab >= self.tabs.len() {
                    self.sel_tab = self.tabs.len().saturating_sub(1);
                }
            }
            Err(e) => self.status = format!("刷新失败: {e}"),
        }
    }
}

fn main() -> io::Result<()> {
    // 支持 `wezlauncher --page new` 直接打开新建页
    let start_new = std::env::args().any(|a| a == "--page");
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, start_new);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, start_on_new: bool) -> io::Result<()> {
    let mut app = App::new(start_on_new);
    ensure_ssh_example();

    loop {
        terminal.draw(|f| draw(f, &mut app))?;
        if !event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.quit = true;
            }
            KeyCode::Tab => {
                app.page = if app.page == Page::Tabs {
                    Page::New
                } else {
                    Page::Tabs
                };
            }
            KeyCode::Char('r') => app.reload(),
            KeyCode::Down | KeyCode::Char('j') => match app.page {
                Page::Tabs => {
                    if app.sel_tab + 1 < app.tabs.len() {
                        app.sel_tab += 1;
                    }
                }
                Page::New => {
                    if app.sel_new + 1 < app.new_items.len() {
                        app.sel_new += 1;
                    }
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match app.page {
                Page::Tabs => app.sel_tab = app.sel_tab.saturating_sub(1),
                Page::New => app.sel_new = app.sel_new.saturating_sub(1),
            },
            KeyCode::Enter => match app.page {
                Page::Tabs => {
                    if let Some(tab) = app.tabs.get(app.sel_tab) {
                        let id = tab.tab_id.to_string();
                        match cli(&["activate-tab", "--tab-id", &id]) {
                            Ok(_) => app.quit = true,
                            Err(e) => app.status = format!("切换失败: {e}"),
                        }
                    }
                }
                Page::New => {
                    if let Some(item) = app.new_items.get(app.sel_new) {
                        let mut args: Vec<&str> = vec!["spawn"];
                        args.extend(item.args.iter().map(|s| s.as_str()));
                        match cli(&args) {
                            Ok(_) => app.quit = true,
                            Err(e) => app.status = format!("新建失败: {e}"),
                        }
                    }
                }
            },
            _ => {}
        }
        if app.quit {
            break;
        }
    }

    // 关掉自己所在的标签页（动作已经执行完了）
    if let Some(pane) = own_pane() {
        let id = pane.to_string();
        let _ = cli(&["kill-pane", "--pane-id", &id]);
    }
    Ok(())
}

fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(f.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(chunks[1]);

    // 顶部
    let page_span = match app.page {
        Page::Tabs => Span::styled(
            " 标签页 ",
            Style::default().fg(Color::Black).bg(Color::LightBlue),
        ),
        Page::New => Span::styled(
            " 新建 ",
            Style::default().fg(Color::Black).bg(Color::LightGreen),
        ),
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " wezlauncher ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            page_span,
            Span::styled(
                format!("   {}", app.status),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        chunks[0],
    );

    // 左：列表
    match app.page {
        Page::Tabs => {
            let items: Vec<ListItem> = app
                .tabs
                .iter()
                .map(|t| {
                    let mark = Span::styled(" ", Style::default());
                    let mut spans = vec![
                        mark,
                        Span::raw(format!("{} ", t.index + 1)),
                        Span::raw(t.title.clone()),
                    ];
                    if t.panes > 1 {
                        spans.push(Span::styled(
                            format!("  [{} 分屏]", t.panes),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    if t.zoomed {
                        spans.push(Span::styled(" [z]", Style::default().fg(Color::DarkGray)));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" 标签页 "))
                .highlight_style(
                    Style::default()
                        .bg(Color::LightBlue)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ ");
            let mut st = ListState::default();
            if !app.tabs.is_empty() {
                st.select(Some(app.sel_tab));
            }
            f.render_stateful_widget(list, body[0], &mut st);
        }
        Page::New => {
            let items: Vec<ListItem> = app
                .new_items
                .iter()
                .map(|i| {
                    ListItem::new(Line::from(vec![
                        Span::raw(format!(" {}  ", i.label)),
                        Span::styled(i.hint.clone(), Style::default().fg(Color::DarkGray)),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" 新建 "))
                .highlight_style(
                    Style::default()
                        .bg(Color::LightGreen)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ ");
            let mut st = ListState::default();
            st.select(Some(app.sel_new));
            f.render_stateful_widget(list, body[1].union(body[0]), &mut st);
        }
    }

    // 右：详情
    if app.page == Page::Tabs {
        let mut lines: Vec<Line> = Vec::new();
        if let Some(t) = app.tabs.get(app.sel_tab) {
            lines.push(Line::from(Span::styled(
                t.title.clone(),
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("窗口 ", Style::default().fg(Color::DarkGray)),
                Span::raw(t.window_id.to_string()),
                Span::styled("   标签 ", Style::default().fg(Color::DarkGray)),
                Span::raw(t.tab_id.to_string()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("目录 ", Style::default().fg(Color::DarkGray)),
                Span::raw(basename(&t.cwd.replace("file:///", ""))),
            ]));
            lines.push(Line::from(vec![
                Span::styled("分屏 ", Style::default().fg(Color::DarkGray)),
                Span::raw(t.panes.to_string()),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                t.cwd.replace("file:///", ""),
                Style::default().fg(Color::Gray),
            )));
        } else {
            lines.push(Line::from("(没有其它标签页，按 Tab 去新建一个)"));
        }
        f.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(" 详情 "))
                .wrap(Wrap { trim: true }),
            body[1],
        );
    }

    // 底部帮助
    let help = Line::from(vec![
        Span::styled(" Tab ", Style::default().fg(Color::LightCyan)),
        Span::raw("切页  "),
        Span::styled("↑↓/jk ", Style::default().fg(Color::LightBlue)),
        Span::raw("选择  "),
        Span::styled("Enter ", Style::default().fg(Color::LightBlue)),
        Span::raw(match app.page {
            Page::Tabs => "切换到该标签页  ",
            Page::New => "打开  ",
        }),
        Span::styled("r ", Style::default().fg(Color::LightBlue)),
        Span::raw("刷新  "),
        Span::styled("Esc ", Style::default().fg(Color::LightBlue)),
        Span::raw("取消  "),
        Span::styled("q ", Style::default().fg(Color::LightBlue)),
        Span::raw("退出"),
    ]);
    f.render_widget(Paragraph::new(help), chunks[2]);
}
