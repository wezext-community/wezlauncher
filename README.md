# wezlauncher

替代 wezterm 内置启动器的 TUI：**切标签页** + **开新 shell**。

新 shell 列表自动包含：

- **所有已安装的 WSL 发行版**（运行时 `wsl.exe -l -q` 枚举，UTF-16LE 解码，过滤 docker-desktop）
- **`~/.wezterm-ssh.txt` 里自定义的 SSH 条目**（首次运行自动生成模板）
- 固定的 PowerShell / CMD / Git Bash / 新窗口

## 用法

```
Ctrl+Shift+O   打开（wezterm 键位里配的是新开一个标签页跑本程序）
Tab            切页：标签页 / 新建
↑↓ / jk        选择
Enter          切换标签页 / 打开新 shell
r              刷新（重读标签页、WSL、SSH）
Esc / q        退出（会把自己这个标签页关掉）
```

也可以 `wezlauncher --page new` 直接打开「新建」页。

## SSH 配置

`C:\Users\<你>\.wezterm-ssh.txt`：

```ini
# 名字 = ssh 参数
家里 = 192.168.1.10
生产 = deploy@example.com -p 2222
跳板 = -i C:/Users/10678/.ssh/id_ed25519 user@10.0.0.5
```

## 依赖

`wezterm cli`（同版本 wezterm 自带）、`wsl.exe`（可选）、`ssh.exe`（可选）。

## 安装

```bash
wezext install wezlauncher
```
