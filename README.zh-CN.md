# wezlauncher

[English](README.md) | **中文**

[WezTerm](https://wezterm.org) 的启动器 TUI —— 替代内置启动器，两个页面：**切换标签页**
和**打开新 shell**。

「新建」列表是运行时生成的，自动包含：

* **所有已安装的 WSL 发行版**（`wsl.exe -l -q` 枚举、UTF-16LE 解码、过滤 `docker-desktop*`），
* **`~/.wezterm-ssh.txt` 里你自己配的 SSH 条目**（首次运行自动生成模板），
* 固定的 PowerShell / CMD / Git Bash / 新窗口项。

## 用法

```
Ctrl+Shift+O   打开（WezTerm 的键位会用新标签页跑它）
Tab            切页：标签页 / 新建
↑↓ 或 jk       移动
Enter          切换标签页 / 启动 shell
r              刷新（标签页、WSL 列表、SSH 条目）
Esc 或 q       退出（会把自己这个标签页关掉）
```

`wezlauncher --page new` 可以直接从「新建」页开始。

## SSH 条目

`C:\Users\<你>\.wezterm-ssh.txt`：

```ini
# 名字 = ssh 参数
家里   = 192.168.1.10
生产   = deploy@example.com -p 2222
跳板   = -i C:/Users/you/.ssh/id_ed25519 user@10.0.0.5
```

`=` 右边的内容原样传给 `ssh.exe`，所以端口、私钥、`-J`、`-L` 转发都能用。

## 和 WezTerm 的交互方式

它通过 `wezterm cli`（列标签页、切标签、开新标签、关自己的标签）工作，
因此必须在 WezTerm 的窗格里运行。

## 安装

```bash
wezext install wezlauncher
```

## 构建

```bash
cargo build --release
# -> target/release/wezlauncher.exe
```
