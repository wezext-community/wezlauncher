// ============================================================
//  wezlauncher —— 替代 wezterm 内置启动器的 TUI
//
//  三个页面（Tab 切换）：
//    1. 标签页  —— 切换当前窗口里的标签页
//    2. 新建    —— PowerShell / CMD / Git Bash / 所有 WSL 发行版 / 自定义条目
//    3. 管理    —— 增删改自定义条目（SSH、WSL 参数、任意命令）
//
//  自定义条目存在 ~/.wezterm-ssh.txt，每行：
//      ssh 名字 = user@host -p 2222
//      wsl 名字 = -d Debian -u root --cd ~
//      cmd 名字 = pwsh.exe -NoLogo -Command "cd C:\\proj"
//  （不带类型前缀时按 ssh 处理，兼容老文件）
//
//  通过 `wezterm cli` 操作标签页，最后关掉自己所在的标签页。
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
    // 在 wezterm-gui 里运行时 WEZTERM_EXECUTABLE 指向 wezterm-gui.exe，
    // 而 `cli` 子命令在 wezterm.exe 里 —— 先找同目录的 wezterm.exe。
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

fn own_pane() -> Option<u64> {
    std::env::var("WEZTERM_PANE").ok()?.parse().ok()
}

// ---------------- 标签页 ----------------

#[derive(Clone)]
struct TabInfo {
    tab_id: u64,
    window_id: u64,
    index: usize,
    title: String,
    cwd: String,
    panes: usize,
    zoomed: bool,
}

fn basename(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if last.is_empty() || last.len() == 2 {
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
    let own_entry = own.and_then(|pid| panes.iter().find(|p| p["pane_id"].as_u64() == Some(pid)));
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
                continue;
            }
        }
        let title = {
            let tt = pane["tab_title"].as_str().unwrap_or("");
            let raw = if tt.is_empty() {
                pane["title"].as_str().unwrap_or("").to_string()
            } else {
                tt.to_string()
            };
            let bs = char::from(92u8);
            if raw.contains('/') || raw.contains(bs) {
                basename(&raw.replace(bs, "/"))
            } else {
                raw
            }
        };
        let cwd = pane["cwd"].as_str().unwrap_or("").to_string();
        let zoomed = pane["is_zoomed"].as_bool().unwrap_or(false);
        match tabs.iter_mut().find(|t| t.tab_id == tab_id) {
            Some(t) => t.panes += 1,
            None => tabs.push(TabInfo {
                tab_id,
                window_id,
                index: tabs.len(),
                title,
                cwd,
                panes: 1,
                zoomed,
            }),
        }
    }
    tabs.sort_by_key(|t| (t.window_id, t.tab_id));
    for (i, t) in tabs.iter_mut().enumerate() {
        t.index = i;
    }
    Ok(tabs)
}

// ---------------- 自定义条目 ----------------

#[derive(Clone)]
struct FileEntry {
    /// ssh | wsl | cmd
    kind: String,
    label: String,
    args: String,
}

impl FileEntry {
    fn to_line(&self) -> String {
        format!("{} {} = {}", self.kind, self.label, self.args)
    }
}

fn entries_path() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".wezterm-ssh.txt")
}

/// 解析一行：`ssh 名字 = 参数` / `wsl 名字 = 参数` / `cmd 名字 = 程序 参数`
fn parse_entry_line(line: &str) -> Option<FileEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (kind, rest) = match line.split_once(char::is_whitespace) {
        Some((k, r)) if matches!(k.to_lowercase().as_str(), "ssh" | "wsl" | "cmd") => {
            (k.to_lowercase(), r)
        }
        _ => ("ssh".to_string(), line),
    };
    let (label, args) = rest.split_once('=')?;
    let label = label.trim();
    let args = args.trim();
    if label.is_empty() || args.is_empty() {
        return None;
    }
    Some(FileEntry {
        kind,
        label: label.to_string(),
        args: args.to_string(),
    })
}

fn load_entries() -> Vec<FileEntry> {
    fs::read_to_string(entries_path())
        .map(|t| t.lines().filter_map(parse_entry_line).collect())
        .unwrap_or_default()
}

fn save_entries(entries: &[FileEntry]) -> io::Result<()> {
    let mut body = String::from(
        "# wezlauncher 的自定义条目\n\
         # 每行： <类型> 名字 = 参数\n\
         #   ssh 家里 = 192.168.1.10 -p 2222\n\
         #   wsl root = -d Debian -u root --cd ~\n\
         #   cmd 项目 = pwsh.exe -NoLogo -Command \"cd C:/proj\"\n\
         # 不带类型前缀时按 ssh 处理\n",
    );
    for e in entries {
        body.push_str(&e.to_line());
        body.push('\n');
    }
    // 先写临时文件再改名，避免读到半截
    let path = entries_path();
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)
}

fn ensure_entries_example() {
    if !entries_path().exists() {
        let _ = save_entries(&[]);
    }
}

// ---------------- 新建页的条目 ----------------

struct NewItem {
    label: String,
    hint: String,
    args: Vec<String>,
}

/// 按 shell 习惯切分参数：支持 "..." / '...' 引号，引号里的空格不切
fn split_args(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for c in s.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                started = true;
            }
            Some(_) => cur.push(c),
            None if c == '"' || c == char::from(39u8) => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn list_wsl() -> Vec<String> {
    let Ok(out) = Command::new("wsl.exe").args(["-l", "-q"]).output() else {
        return Vec::new();
    };
    let units: Vec<u16> = out
        .stdout
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_start_matches('\u{feff}')
        .lines()
        .map(|l| l.trim().trim_end_matches('\0').to_string())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.to_lowercase().starts_with("docker-desktop"))
        .collect()
}

fn new_items(entries: &[FileEntry]) -> Vec<NewItem> {
    let mk = |label: &str, hint: &str, args: &[&str]| NewItem {
        label: label.to_string(),
        hint: hint.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    };

    let mut items = vec![
        mk("PowerShell 7 标签页", "Windows", &["--", "pwsh.exe", "-NoLogo"]),
        mk("CMD 标签页", "Windows", &["--", "cmd.exe"]),
        mk(
            "Git Bash 标签页",
            "Windows",
            &["--", "C:/Program Files/Git/bin/bash.exe", "-li"],
        ),
    ];

    for distro in list_wsl() {
        items.push(NewItem {
            label: format!("WSL: {distro}"),
            hint: "WSL 家目录".to_string(),
            args: vec![
                "--".into(),
                "wsl.exe".into(),
                "-d".into(),
                distro,
                "--cd".into(),
                "~".into(),
            ],
        });
    }

    for e in entries {
        let mut args = vec!["--".to_string()];
        match e.kind.as_str() {
            "wsl" => {
                args.push("wsl.exe".to_string());
                args.extend(split_args(&e.args));
            }
            "cmd" => args.extend(split_args(&e.args)),
            _ => {
                args.push("ssh.exe".to_string());
                args.extend(split_args(&e.args));
            }
        }
        let kind_tag = match e.kind.as_str() {
            "wsl" => "WSL",
            "cmd" => "CMD",
            _ => "SSH",
        };
        items.push(NewItem {
            label: format!("{kind_tag}: {}", e.label),
            hint: e.args.clone(),
            args,
        });
    }

    items.push(mk(
        "新窗口（PowerShell）",
        "独立窗口",
        &["--new-window", "--", "pwsh.exe", "-NoLogo"],
    ));

    items
}

// ---------------- 应用状态 ----------------

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Tabs,
    New,
    Manage,
}

#[derive(Default)]
struct Input {
    /// 按 char 存，光标位置 pos 是 char 下标（0..=chars.len()）
    chars: Vec<char>,
    pos: usize,
    /// Some(idx) 表示在编辑已有条目，None 表示新增
    editing: Option<usize>,
}

impl Input {
    fn new(text: &str, editing: Option<usize>) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let pos = chars.len();
        Self {
            chars,
            pos,
            editing,
        }
    }

    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    fn insert(&mut self, c: char) {
        self.chars.insert(self.pos, c);
        self.pos += 1;
    }

    fn backspace(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
            self.chars.remove(self.pos);
        }
    }

    fn delete(&mut self) {
        if self.pos < self.chars.len() {
            self.chars.remove(self.pos);
        }
    }

    fn left(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    fn right(&mut self) {
        if self.pos < self.chars.len() {
            self.pos += 1;
        }
    }

    fn home(&mut self) {
        self.pos = 0;
    }

    fn end(&mut self) {
        self.pos = self.chars.len();
    }

    /// 删掉光标前的一个词（Ctrl+W）
    fn delete_word_back(&mut self) {
        while self.pos > 0 && self.chars[self.pos - 1].is_whitespace() {
            self.backspace();
        }
        while self.pos > 0 && !self.chars[self.pos - 1].is_whitespace() {
            self.backspace();
        }
    }

    /// 拆成 (光标前, 光标处字符, 光标后)
    fn split_at_cursor(&self) -> (String, Option<char>, String) {
        let before: String = self.chars[..self.pos].iter().collect();
        let at = self.chars.get(self.pos).copied();
        let after: String = self.chars[(self.pos + 1).min(self.chars.len())..]
            .iter()
            .collect();
        (before, at, after)
    }
}

struct App {
    tabs: Vec<TabInfo>,
    entries: Vec<FileEntry>,
    new_items: Vec<NewItem>,
    sel_tab: usize,
    sel_new: usize,
    sel_entry: usize,
    page: Page,
    input: Option<Input>,
    status: String,
    quit: bool,
}

impl App {
    fn new(start_page: Page) -> Self {
        let (tabs, status) = match load_tabs() {
            Ok(t) => {
                let s = format!("{} 个标签页（Esc 取消）", t.len());
                (t, s)
            }
            Err(e) => (Vec::new(), format!("读取标签页失败: {e}")),
        };
        let entries = load_entries();
        // 调试用：WEZLAUNCHER_INPUT="ssh a = b" 启动即进入输入模式
        let input = std::env::var("WEZLAUNCHER_INPUT").ok().map(|t| {
            let mut i = Input::new(&t, None);
            // WEZLAUNCHER_INPUT_LEFT=N 把光标往左挪 N 格，方便截图验证
            let n: usize = std::env::var("WEZLAUNCHER_INPUT_LEFT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            for _ in 0..n {
                i.left();
            }
            i
        });
        App {
            tabs,
            new_items: new_items(&entries),
            entries,
            sel_tab: 0,
            sel_new: 0,
            sel_entry: 0,
            page: start_page,
            input,
            status,
            quit: false,
        }
    }

    fn reload(&mut self) {
        self.entries = load_entries();
        self.new_items = new_items(&self.entries);
        match load_tabs() {
            Ok(t) => {
                self.tabs = t;
                self.status = format!("已刷新：{} 个标签页，{} 个自定义条目", self.tabs.len(), self.entries.len());
                if self.sel_tab >= self.tabs.len() {
                    self.sel_tab = self.tabs.len().saturating_sub(1);
                }
            }
            Err(e) => self.status = format!("刷新失败: {e}"),
        }
        if self.sel_entry >= self.entries.len() {
            self.sel_entry = self.entries.len().saturating_sub(1);
        }
    }

    fn commit_input(&mut self) {
        let Some(input) = self.input.take() else { return };
        let text = input.text().trim().to_string();
        if text.is_empty() {
            self.status = "取消：内容为空".into();
            return;
        }
        let Some(entry) = parse_entry_line(&text) else {
            self.status = "格式不对，应形如 `ssh 名字 = user@host -p 2222`".into();
            return;
        };
        match input.editing {
            Some(idx) if idx < self.entries.len() => {
                self.status = format!("已修改：{}", entry.to_line());
                self.entries[idx] = entry;
            }
            _ => {
                self.status = format!("已新增：{}", entry.to_line());
                self.entries.push(entry);
                self.sel_entry = self.entries.len().saturating_sub(1);
            }
        }
        self.persist();
    }

    fn delete_selected_entry(&mut self) {
        if self.sel_entry >= self.entries.len() {
            return;
        }
        let removed = self.entries.remove(self.sel_entry);
        self.status = format!("已删除：{}", removed.to_line());
        if self.sel_entry >= self.entries.len() {
            self.sel_entry = self.entries.len().saturating_sub(1);
        }
        self.persist();
    }

    fn persist(&mut self) {
        match save_entries(&self.entries) {
            Ok(()) => {
                self.new_items = new_items(&self.entries);
            }
            Err(e) => self.status = format!("写入失败: {e}"),
        }
    }
}

// ---------------- 主循环 ----------------

fn parse_start_page() -> Page {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().position(|a| a == "--page") {
        Some(i) => match args.get(i + 1).map(|s| s.as_str()) {
            Some("new") => Page::New,
            Some("manage") => Page::Manage,
            _ => Page::Tabs,
        },
        None => Page::Tabs,
    }
}

fn main() -> io::Result<()> {
    ensure_entries_example();
    let start = parse_start_page();
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, start);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, start_page: Page) -> io::Result<()> {
    let mut app = App::new(start_page);

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

        // 输入模式优先（支持左右键、Home/End、Delete、Ctrl+W 等）
        if let Some(input) = app.input.as_mut() {
            use ratatui::crossterm::event::KeyModifiers as KM;
            let ctrl = key.modifiers.contains(KM::CONTROL);
            match key.code {
                KeyCode::Enter => app.commit_input(),
                KeyCode::Esc => {
                    app.input = None;
                    app.status = "已取消输入".into();
                }
                KeyCode::Left => input.left(),
                KeyCode::Right => input.right(),
                KeyCode::Home => input.home(),
                KeyCode::End => input.end(),
                KeyCode::Backspace => input.backspace(),
                KeyCode::Delete => input.delete(),
                KeyCode::Char('a') if ctrl => input.home(),
                KeyCode::Char('e') if ctrl => input.end(),
                KeyCode::Char('u') if ctrl => {
                    input.home();
                    while !input.is_empty() {
                        input.delete();
                    }
                }
                KeyCode::Char('w') if ctrl => input.delete_word_back(),
                KeyCode::Char(c) if !ctrl => input.insert(c),
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.quit = true,
            KeyCode::Tab => {
                app.page = match app.page {
                    Page::Tabs => Page::New,
                    Page::New => Page::Manage,
                    Page::Manage => Page::Tabs,
                };
                app.status = match app.page {
                    Page::Tabs => format!("{} 个标签页", app.tabs.len()),
                    Page::New => format!("{} 个可启动项", app.new_items.len()),
                    Page::Manage => format!("{} 个自定义条目（a 新增 / Enter 编辑 / d 删除）", app.entries.len()),
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
                Page::Manage => {
                    if app.sel_entry + 1 < app.entries.len() {
                        app.sel_entry += 1;
                    }
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match app.page {
                Page::Tabs => app.sel_tab = app.sel_tab.saturating_sub(1),
                Page::New => app.sel_new = app.sel_new.saturating_sub(1),
                Page::Manage => app.sel_entry = app.sel_entry.saturating_sub(1),
            },
            KeyCode::Char('a') if app.page == Page::Manage => {
                app.input = Some(Input::new("", None));
                app.status = "新增：输入 `ssh 名字 = 参数` 后回车".into();
            }
            KeyCode::Char('d') if app.page == Page::Manage => {
                if !app.entries.is_empty() {
                    app.delete_selected_entry();
                }
            }
            KeyCode::Char('e') if app.page == Page::Manage => {
                if let Some(e) = app.entries.get(app.sel_entry) {
                    app.input = Some(Input::new(&e.to_line(), Some(app.sel_entry)));
                    app.status = "编辑中：改完回车保存，Esc 取消".into();
                }
            }
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
                Page::Manage => {
                    if let Some(e) = app.entries.get(app.sel_entry) {
                        app.input = Some(Input::new(&e.to_line(), Some(app.sel_entry)));
                        app.status = "编辑中：改完回车保存，Esc 取消".into();
                    }
                }
            },
            _ => {}
        }
        if app.quit {
            break;
        }
    }

    if let Some(pane) = own_pane() {
        let id = pane.to_string();
        let _ = cli(&["kill-pane", "--pane-id", &id]);
    }
    Ok(())
}

// ---------------- 绘制 ----------------

fn page_badge(page: Page) -> Span<'static> {
    match page {
        Page::Tabs => Span::styled(" 标签页 ", Style::default().fg(Color::Black).bg(Color::LightBlue)),
        Page::New => Span::styled(" 新建 ", Style::default().fg(Color::Black).bg(Color::LightGreen)),
        Page::Manage => Span::styled(" 管理 ", Style::default().fg(Color::Black).bg(Color::LightYellow)),
    }
}

fn kind_color(kind: &str) -> Color {
    match kind {
        "wsl" => Color::LightMagenta,
        "cmd" => Color::LightBlue,
        _ => Color::LightGreen,
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(if app.input.is_some() { 3 } else { 2 }),
        ])
        .split(f.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(chunks[1]);

    // 顶部
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
            page_badge(app.page),
            Span::styled(
                format!("   {}", app.status),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        chunks[0],
    );

    // 列表
    match app.page {
        Page::Tabs => {
            let items: Vec<ListItem> = app
                .tabs
                .iter()
                .map(|t| {
                    let mut spans = vec![
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

            // 右侧详情
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
                lines.push(Line::from("(没有其它标签页，Tab 去新建一个)"));
            }
            f.render_widget(
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title(" 详情 "))
                    .wrap(Wrap { trim: true }),
                body[1],
            );
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
        Page::Manage => {
            let items: Vec<ListItem> = if app.entries.is_empty() {
                vec![ListItem::new(Line::from(Span::styled(
                    " (还没有自定义条目，按 a 新增一条)",
                    Style::default().fg(Color::DarkGray),
                )))]
            } else {
                app.entries
                    .iter()
                    .map(|e| {
                        ListItem::new(Line::from(vec![
                            Span::styled(
                                format!(" {:<3} ", e.kind.to_uppercase()),
                                Style::default()
                                    .fg(kind_color(&e.kind))
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::raw(e.label.clone()),
                            Span::styled(
                                format!("   {}", e.args),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]))
                    })
                    .collect()
            };
            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" 管理自定义条目（a 新增 / Enter 编辑 / d 删除） "),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::LightYellow)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ ");
            let mut st = ListState::default();
            if !app.entries.is_empty() {
                st.select(Some(app.sel_entry));
            }
            f.render_stateful_widget(list, body[1].union(body[0]), &mut st);
        }
    }

    // 底部：帮助 + 输入行
    let help = if app.input.is_some() {
        Line::from(vec![
            Span::styled(" 输入 ", Style::default().fg(Color::Black).bg(Color::LightYellow)),
            Span::raw(" 形如 `ssh 名字 = user@host -p 2222`，Enter 保存，Esc 取消"),
        ])
    } else {
        Line::from(vec![
            Span::styled(" Tab ", Style::default().fg(Color::LightCyan)),
            Span::raw("切页  "),
            Span::styled("↑↓/jk ", Style::default().fg(Color::LightBlue)),
            Span::raw("选择  "),
            Span::styled("Enter ", Style::default().fg(Color::LightBlue)),
            Span::raw(match app.page {
                Page::Tabs => "切换到该标签页  ",
                Page::New => "打开  ",
                Page::Manage => "编辑选中条目  ",
            }),
            Span::styled("r ", Style::default().fg(Color::LightBlue)),
            Span::raw("刷新  "),
            Span::styled("Esc ", Style::default().fg(Color::LightBlue)),
            Span::raw("取消  "),
            Span::styled("q ", Style::default().fg(Color::LightBlue)),
            Span::raw("退出"),
        ])
    };
    f.render_widget(Paragraph::new(help), chunks[2]);

    if let Some(input) = app.input.as_ref() {
        let area = ratatui::layout::Rect {
            x: chunks[2].x,
            y: chunks[2].y + 1,
            width: chunks[2].width,
            height: 1,
        };
        let (before, at, after) = input.split_at_cursor();
        let cursor = Span::styled(
            match at {
                Some(c) => c.to_string(),
                None => " ".to_string(),
            },
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::LightGreen)),
                Span::raw(before),
                cursor,
                Span::raw(after),
            ])),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_without_prefix() {
        let e = parse_entry_line("家里 = 192.168.1.10 -p 2222").unwrap();
        assert_eq!(e.kind, "ssh");
        assert_eq!(e.label, "家里");
        assert_eq!(e.args, "192.168.1.10 -p 2222");
    }

    #[test]
    fn parse_typed_entries() {
        let e = parse_entry_line("wsl root = -d Debian -u root --cd ~").unwrap();
        assert_eq!(e.kind, "wsl");
        assert_eq!(e.label, "root");

        let e = parse_entry_line("cmd 项目 = pwsh.exe -NoLogo").unwrap();
        assert_eq!(e.kind, "cmd");
        assert_eq!(e.label, "项目");
    }

    #[test]
    fn skip_comments_and_bad_lines() {
        assert!(parse_entry_line("# ssh 注释 = 忽略").is_none());
        assert!(parse_entry_line("   ").is_none());
        assert!(parse_entry_line("没有等号").is_none());
    }

    #[test]
    fn round_trip_line() {
        let e = parse_entry_line("ssh prod = deploy@example.com -p 2222").unwrap();
        let line = e.to_line();
        assert_eq!(line, "ssh prod = deploy@example.com -p 2222");
        let again = parse_entry_line(&line).unwrap();
        assert_eq!(again.label, e.label);
        assert_eq!(again.args, e.args);
        assert_eq!(again.kind, e.kind);
    }

    #[test]
    fn split_args_keeps_quotes() {
        let v = split_args(r#"pwsh.exe -NoLogo -Command "cd C:/proj""#);
        assert_eq!(v[0], "pwsh.exe");
        assert_eq!(v.len(), 4);
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn cursor_moves_and_inserts_at_position() {
        let mut i = Input::new("abc", None);
        i.left();
        i.insert('X');
        assert_eq!(i.text(), "abXc");
        assert_eq!(i.pos, 3);
    }

    #[test]
    fn backspace_and_delete() {
        let mut i = Input::new("abc", None);
        i.backspace();
        assert_eq!(i.text(), "ab");
        i.home();
        i.delete();
        assert_eq!(i.text(), "b");
    }

    #[test]
    fn home_end_and_arrows_clamp() {
        let mut i = Input::new("abc", None);
        i.home();
        assert_eq!(i.pos, 0);
        i.left();
        assert_eq!(i.pos, 0);
        i.end();
        assert_eq!(i.pos, 3);
        i.right();
        assert_eq!(i.pos, 3);
    }

    #[test]
    fn delete_word_back() {
        let mut i = Input::new("ssh 家里 = 1.2.3.4", None);
        i.delete_word_back();
        assert_eq!(i.text(), "ssh 家里 = ");
    }

    #[test]
    fn utf8_cursor_is_char_based() {
        let mut i = Input::new("中文", None);
        i.left();
        i.insert('X');
        assert_eq!(i.text(), "中X文");
        i.backspace();
        assert_eq!(i.text(), "中文");
    }

    #[test]
    fn split_at_cursor_shows_cursor_char() {
        let mut i = Input::new("ab", None);
        i.left();
        let (before, at, after) = i.split_at_cursor();
        assert_eq!(before, "a");
        assert_eq!(at, Some('b'));
        assert_eq!(after, "");
    }
}
