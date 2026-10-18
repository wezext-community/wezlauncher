# wezlauncher

**English** | [中文](README.zh-CN.md)

A launcher TUI for [WezTerm](https://wezterm.org) — a replacement for the built-in
launcher, with two pages: **switch tabs** and **open a new shell**.

The "new shell" list is built at runtime and automatically contains:

* **every installed WSL distribution** (enumerated with `wsl.exe -l -q`, UTF-16LE
  decoded, `docker-desktop*` filtered out),
* **your own SSH entries** from `~/.wezterm-ssh.txt` (a template is created on first run),
* the fixed PowerShell / CMD / Git Bash / new-window items.

## Usage

```
Ctrl+Shift+O   open it (the WezTerm key binding spawns it in a new tab)
Tab            switch page: tabs / new
↑↓ or jk       move
Enter          activate the tab / launch the shell
r              refresh (tabs, WSL list, SSH entries)
Esc or q       quit (it closes its own tab)
```

`wezlauncher --page new` starts directly on the "new" page.

## SSH entries

`C:\Users\<you>\.wezterm-ssh.txt`:

```ini
# name = ssh arguments
home   = 192.168.1.10
prod   = deploy@example.com -p 2222
jump   = -i C:/Users/you/.ssh/id_ed25519 user@10.0.0.5
```

Everything after `=` is passed to `ssh.exe` verbatim, so ports, keys, `-J`, `-L` … all work.

## How it talks to WezTerm

It uses `wezterm cli` (list tabs, activate a tab, spawn a tab, kill its own pane),
so it must run inside a WezTerm pane.

## Install

```bash
wezext install wezlauncher
```

## Build

```bash
cargo build --release
# -> target/release/wezlauncher.exe
```
