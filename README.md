# BgPicker：壁纸选择、实时 GPU 模糊背景与视差引擎

适用于 **niri**（Wayland）的壁纸工具。`rust` 分支将静态壁纸、动态视频壁纸、视差排版、换图过渡着色器与概览实时模糊整合进统一的 Rust 守护进程 `bgpicker-daemon`，并完整保留轻量级 **Quickshell** 双页倾斜卡片选择器界面；简洁模式使用两个受管理的 `swaybg` 实例极简运行。

| 引擎 / 模式 | 清晰前景（`bgpicker:wallpaper`） | 概览模糊后景（`bgpicker:backdrop`） | 渲染后端 |
| --- | --- | --- | --- |
| `full` / `awww`（默认） | 原图居中铺满 | 原图 3-pass Dual-Kawase 实时／图像空间缓存模糊 | `bgpicker-daemon` (EGL + GLES) |
| `full` / `parallax` | 跟随 niri 焦点窗口列水平视差平移 | 同一套硬件渲染，默认随前景同步视差（支持 `--keep-backdrop`／独立开关） | `bgpicker-daemon` (EGL + GLES) |
| `video` / `normal` | 硬件优先异步单解码器播放 | 单流共享，仅在概览打开与切入预热期实时 Dual-Kawase 模糊 | `bgpicker-daemon` + `bgpicker-video` |
| `video` / `parallax` | 视频跟随 niri 焦点列水平视差平移 | 单流共享，默认同步视差或独立控制 | `bgpicker-daemon` + `bgpicker-video` |
| `simple` | 仅更改 namespace 的本地 `swaybg` 副本（`bgpick-fg`） | `swaybg`（`^wallpaper$`）加载预生成静态模糊图 | 双 `swaybg`（退出守护进程与选择器） |

> 注：命令行与配置中保留 `awww` 作为静态“普通居中填充”模式的名称以保持兼容，实际绘制与过渡特效均由 `bgpicker-daemon` 内部 OpenGL ES 管线完成，不再依赖外部 `awww-daemon` 进程。

## 视频壁纸与双页面选择器

将 MP4、MKV、WebM 或 MOV 视频放在 `dynamic/`；将 JPG、JPEG、PNG、WEBP、GIF、BMP、JXL、AVIF、HEIF 或 EXR 图片放在 `wallpapers/`（个人媒体文件默认被 Git 忽略）。

```bash
./bgpicker.sh video normal             # 普通视频：清晰前景与概览模糊后景
./bgpicker.sh video parallax           # 视频视差（默认同步开启模糊后景视差）
./bgpicker.sh video parallax "/绝对路径/视频.mp4"
./bgpicker.sh video next               # 按文件名顺序切换 dynamic/ 中的视频
./bgpicker.sh video                    # 查看当前引擎、视频模式与文件
./bgpicker.sh engine full              # 恢复完整静态壁纸模式
./bgpicker.sh engine simple            # 切换到双 swaybg 简洁模式
./bgpicker.sh status                   # 查看守护进程与输出状态
./bgpicker.sh clear-cache [--deep] [--picker] [--disk] [--all]  # 手动清理空闲缓存与内存
./bgpicker_stats.sh [-w [秒数]]        # 一键抓取 bgpicker-daemon 与 qs 的 CPU/PSS/VRAM 占用
```

也可以直接使用编译好的 `bgpickerctl` 客户端（位于 `target/release/bgpickerctl` 或环境变量 `BGPICKERCTL` 指定路径）：

```bash
bgpickerctl status [--json] [--no-start]
bgpickerctl apply-static <图片路径> [--mode awww|parallax] [--keep-backdrop | --backdrop <on|off|keep>]
bgpickerctl apply-video [normal|parallax|toggle] [视频路径] [--keep-backdrop | --backdrop <on|off|keep>]
bgpickerctl mode [awww|parallax|toggle] [--keep-backdrop | --backdrop <on|off|keep>]
bgpickerctl backdrop-parallax [on|off|toggle]
bgpickerctl transition [fade|wipe|wave|grow|outer|random|none]
bgpickerctl engine [status|full|simple|video|toggle]
bgpickerctl reload-settings
bgpickerctl clear-cache [--deep] [--picker] [--disk] [--all]
```

按 `Mod+N`（或 `./bgpicker.sh toggle`）打开选择器面板：
- 向下滚动进入动态壁纸页，向上滚动回到静态壁纸页；两页分别记住选中项，切页不会应用壁纸。
- 静态与动态页目录通过 `bgpickerctl catalog` 扫描生成，统一以 `1280×672` JPG 缩略图缓存至 `~/.cache/bgpicker/{image,video}-thumbnails/`，仅在长按 1.5 秒放大预览或应用时加载原图。
- 面板底部提供“普通／视差”倾斜开关与 7 种切换效果按钮；切换效果偏好全局共用，普通／视差状态在静态与动态之间各自保存。

### 视频解码与实时模糊机制

- **单路解码共享前后景**：`bgpicker-video` 守护线程通过 FFmpeg C FFI（优先自动探测硬件加速与零拷贝，不可用时回退软件解码）解码单路视频并写入三缓冲队列，同时供前景和概览后景使用，不再需要双播放器或 `ffmpeg` 预先转码整段 1080p 模糊视频缓存。
- **GPU Dual-Kawase 模糊与防闪烁缓存**：后景窗口 `bgpicker:backdrop` 通过 3-pass `DualKawaseBlur` 降采样／升采样着色器直接生成模糊图。静态图启用图像空间缓存（按源图计算，最大 1920×1080 约束下的 FBO 链，仅在换图或首帧计算一次），视差平移期间直接裁剪采样，消除移动时的采样闪烁；视频则仅在 `niri` 概览打开期间（以及刚切换视频的前 4 帧预热期）更新模糊金字塔，概览关闭时跳过模糊计算以节省 GPU 资源。
- **切换体验**：切换视频、视频与静态互切，都在同一组 EGL 表面内直接播放 7 种着色器换图动画。切换视频时在后台等待新视频首帧就绪（`min_ready_frames: 2`）再开启过渡，过渡完成后释放旧解码流；静态壁纸按原图首帧纹理加载；`scene.pkg` 与网页壁纸不能直接播放。

## 简洁模式

除完整与视频模式外，可将引擎切换为极简的双 `swaybg` 模式：

```bash
./bgpicker.sh engine simple  # 切换到简洁模式
./bgpicker.sh engine full    # 恢复完整模式（先提交第一帧后再退出 swaybg）
./bgpicker.sh engine toggle  # 在 full 与 simple 之间切换
./bgpicker.sh engine         # 查看当前模式
```

简洁模式通过 `simple_wallpaper.sh` 启动两个 `swaybg`：一个使用系统 `swaybg`（默认 namespace `wallpaper`）绘制概览模糊图，另一个生成仅修改了 Wayland layer namespace（`bgpick-fg`）的本地副本 `~/.cache/bgpicker/swaybg-front` 绘制桌面清晰前景，同时停止 Quickshell 选择器与 `bgpicker-daemon` 守护进程。切换回 `full` 或 `video` 时会先等待 `bgpicker-daemon` 完成首帧提交再安全清理 `swaybg`。

此外，`bgpicker-daemon` 在应用静态壁纸时也会在后台异步更新兼容模糊图（`~/.cache/bgpicker/blur/` 及 `~/.cache/current_swaybg_img`），以便 `simple` 模式切换或锁屏程序（如 `swaylock`）直接复用。

## 构建、依赖与启动

### 依赖清单

- **构建 `bgpicker-daemon` / `bgpickerctl`**：Rust 工具链（`cargo`）、`pkg-config`、C 编译器（`cc`）、FFmpeg 与 DRM 开发库（`libavformat`、`libavcodec`、`libswscale`、`libavutil`、`libdrm`）。
- **完整／视频运行期**：Wayland 合成器（niri 会话提供 `NIRI_SOCKET`）、EGL / OpenGL ES 运行时、`Quickshell`（`qs`，选择器面板）、`ImageMagick`（`magick`，静态缩略图与兼容模糊图）、`ffmpeg` CLI（视频缩略图）、`Bash`。
- **简洁模式补充依赖**：`swaybg`、`Python 3`、`util-linux`（`flock`）。

### 编译

```bash
cargo build --release
```

编译产物位于 `target/release/bgpicker-daemon` 与 `target/release/bgpickerctl`。通过 `./bgpicker.sh` 等包装脚本调用时，若未找到预编译二进制也会自动触发一次 release 构建；也可以运行 `./bgpicker.sh start` 直接拉起守护进程与选择器面板。

### niri 自启动与 Layer 规则

在 `~/.config/niri/config.kdl` 中使用统一入口（移除旧版的 `awww-daemon` 和直接启动 `qs` 的条目）：

```kdl
spawn-at-startup "/home/lightwall/playground/bgpicker/wallpaper_engine.sh" "startup"
```

添加如下 `layer-rule` 将模糊层放入 niri 概览背景（**不要**将 `^bgpicker:wallpaper$` 放入 backdrop）：

```kdl
layer-rule {
    match namespace="^bgpicker:backdrop$"
    match namespace="^wallpaper$" // 简洁模式的模糊 swaybg
    place-within-backdrop true
}
```

## 使用

| 操作 | 入口 |
| --- | --- |
| 显示／关闭选择器 | `./bgpicker.sh toggle` |
| 静态／动态页切换 | 鼠标滚轮上／下 |
| 浏览 | `←` / `→` 或 `H` / `L` |
| 应用／关闭 | `Enter` / `Escape` |
| 搜索文件名 | `Ctrl+F` |
| 首张／末张 | `Home` / `End` |
| 放大预览 | 长按或停留在卡片上按预览触发（原图非缓存加载） |
| 普通／视差模式切换 | 选择器底部中央的倾斜分段选项 |
| 选择换图效果 | 底部右侧的 7 种切换效果按钮，全模式共用；下次应用壁纸时生效 |

命令行操作示例：

```bash
./bgpicker.sh mode                 # 显示已保存的静态模式
./bgpicker.sh mode parallax        # 用当前壁纸启用视差（默认同步开启模糊背景视差）
./bgpicker.sh mode awww            # 恢复居中填充模式（默认同步关闭模糊背景视差）
./bgpicker.sh mode toggle
./bgpicker.sh transition wave      # 保存共用的换图效果
./bgpicker.sh transition           # 查看当前效果及参数
./bgpicker.sh backdrop-parallax on # 独立开启模糊背景视差
./bgpicker.sh backdrop-parallax off
./switch_wallpaper.sh ./wallpapers/A.png
```

若希望切换前景视差时保留模糊背景当前的视差开关状态，或显式覆盖背景模式，可直接通过 `bgpickerctl` 传入参数：

```bash
bgpickerctl mode parallax --keep-backdrop
bgpickerctl apply-static ./wallpapers/A.png --mode parallax --backdrop off
bgpickerctl apply-video parallax ./dynamic/demo.mp4 --backdrop keep
```

单纯应用新图片而不显式指定 `--mode` 时（如 `./switch_wallpaper.sh ./wallpapers/B.png` 或选择器确认换图），会继续保留用户自定义的“前景 + 后景视差”组合状态。

原有选择器 IPC 保持兼容（将 `<project>` 换为项目绝对路径）：

```bash
qs ipc -p <project> call wallpaperCarousel toggle
qs ipc -p <project> call wallpaperCarousel cycleNext
qs ipc -p <project> call wallpaperCarousel cyclePrevious
```

## 换图效果、视差与配置

| 界面名称 | 配置值 | 效果 |
| --- | --- | --- |
| 淡化 | `fade` | 新旧壁纸淡入淡出 |
| 斜扫 | `wipe` | 按角度扫过画面 |
| 波浪 | `wave` | 波浪边缘扫过画面 |
| 圆形展开 | `grow` | 从指定中心向外展开 |
| 圆形收拢 | `outer` | 从外侧向中心收拢 |
| 随机 | `random` | 每次在前五种效果中随机选一种 |
| 直接 | `none` | 立即更换 |

- **同面平滑模式切换**：普通（居中填充）与视差模式切换时，在同一个 EGL 表面内插值更新采样矩形与平移权重（`modeTransitionMs`，默认 `500` ms），无多渲染器交接闪烁。
- **niri 事件驱动**：`bgpicker-daemon` 直接连接 `NIRI_SOCKET` 监听工作区与焦点窗口列变化，按每块输出独立计算视差进度并以 `OutCubic` 缓动推进动画；空闲时重置帧调度锚点并仅按需重绘，不会把闲置时长计入动画第 0 帧。
- **配置热重载**：守护进程通过 `inotify` 监听 `${XDG_CONFIG_HOME:-~/.config}/bgpicker/settings.json`，也可手动执行 `bgpickerctl reload-settings`。

首次使用可复制示例配置（或由面板按钮自动创建）：

```bash
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/bgpicker"
cp -n settings.example.json "${XDG_CONFIG_HOME:-$HOME/.config}/bgpicker/settings.json"
```

参数示例见 `settings.example.json`：

```json
{
  "parallax": {
    "zoom": 1.12,
    "columnSpan": 6,
    "durationMs": 350,
    "modeTransitionMs": 500,
    "maxTextureSize": 8192
  },
  "transition": {
    "type": "fade",
    "durationMs": 800,
    "fps": 60,
    "angle": 45,
    "position": [0.5, 0.5],
    "wave": [20, 20]
  }
}
```

| 参数 | 默认 | 范围／说明 |
| --- | --- | --- |
| `parallax.zoom` | `1.12` | `[1.0, 2.0]` 普通比例图片放大倍率；超宽长图按高度适配并利用自身横向余量 |
| `parallax.columnSpan` | `6` | `[0, 100]` 从第一列移到另一端所需的列数；`0` 表示按当前工作区列数自动铺满 |
| `parallax.durationMs` | `350` | `[0, 3000]` 视差列切换动画时长（毫秒） |
| `parallax.modeTransitionMs` | `500` | `[0, 3000]` 普通／视差模式平滑过渡时长（毫秒）；`0` 为立即切换 |
| `parallax.maxTextureSize` | `8192` | `[1024, 16384]` 静态壁纸解码最大边长 |
| `transition.type` | `fade` | `fade` / `wipe` / `wave` / `grow` / `outer` / `random` / `none` |
| `transition.durationMs` | `800` | `[0, 5000]` 换图过渡时长（毫秒）；`0` 为立即切换 |
| `transition.fps` | `60` | `[1, 240]` 兼容帧率参数 |
| `transition.angle` | `45` | `[0, 360]` 斜扫／波浪方向（度） |
| `transition.position` | `[0.5, 0.5]` | `[0, 1]` 圆形展开／收拢归一化中心 `[x, y]` |
| `transition.wave` | `[20, 20]` | `[1, 500]` 波浪宽度与振幅 |

## 状态、缓存与环境变量

| 路径 | 内容 |
| --- | --- |
| `wallpapers/`、`dynamic/` | 本地静态图片与动态视频原文件 |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/current_wallpaper` | 上次成功应用的静态原图路径 |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/engine` | `full`／`simple`／`video`（默认 `full`） |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/mode` | 完整模式静态偏好：`awww`／`parallax` |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/backdrop_parallax` | 模糊背景视差开关：`on`／`off` |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/video-wallpaper`、`video-mode` | 上次视频路径及 `normal`／`parallax` 模式 |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/simple-{front,back}.pid` | 简洁模式两个 `swaybg` 实例的 PID 与启动时间 |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/daemon.log` | `bgpicker-daemon` 运行日志 |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/image-thumbnails/` | 静态壁纸 `1280×672` JPG 缩略图缓存 |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/video-thumbnails/` | 动态视频 `1280×672` JPG 缩略图缓存 |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/blur/` | 兼容 `simple` 模式与锁屏的离线静态模糊图缓存 |
| `~/.cache/current_swaybg_img` | 指向当前静态兼容模糊图的兼容路径 |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/swaybg-front` | 简洁模式前景 `swaybg` 本地 namespace 副本 |
| `${XDG_RUNTIME_DIR:-/tmp}/bgpicker-${WAYLAND_DISPLAY:-wayland-0}.sock` | 守护进程 UNIX IPC 套接字 |

常用环境变量：
- `BGPICKER_DAEMON` / `BGPICKERCTL`：手动指定 `bgpicker-daemon` / `bgpickerctl` 可执行文件路径。
- `BGPICKER_SOCKET`：覆盖守护进程 IPC 套接字路径。
- `BGPICKER_BLUR_SIZE`（默认 `1920x1080`）、`BGPICKER_BLUR_SIGMA`（默认 `20`）：控制异步兼容离线模糊图尺寸与模糊半径。

## 目录结构与开发验证

- **`crates/bgpicker-core`**：XDG 路径与原子状态读写、配置规范化与随机效果解析、视差/几何数学、`niri` IPC 客户端、守护进程 JSON IPC 协议、媒体目录扫描与缩略图缓存。
- **`crates/bgpicker-gfx`**：`smithay-client-toolkit` + `wayland-egl` + `glow` 双层渲染器（`bgpicker:wallpaper` / `bgpicker:backdrop`）、换图着色器（`fade`/`wipe`/`wave`/`grow`/`outer`）、图像空间缓存 3-pass `DualKawaseBlur`。
- **`crates/bgpicker-video`**：基于 FFmpeg C FFI（`ffi/ffmpeg_shim.c`）的异步视频解码线程与三缓冲帧池，支持硬件加速探测、帧步调控制、双帧预热与首 4 帧背景预热。
- **`crates/bgpicker-daemon`**：单线程序列化 Actor 状态机、`bgpicker-daemon` 守护进程与 `bgpickerctl` CLI、`inotify` 配置监听。
- **前端与兼容脚本**：`shell.qml`、`BgPicker.qml`、`Carousel.qml`、`CarouselMedia.js`、`SkewSelector.qml`、`WallpaperTransitions.js` 保留原面板交互；`bgpicker.sh`、`wallpaper_engine.sh`、`switch_wallpaper.sh`、`picker_action.sh`、`backdrop_parallax.sh`、`backdrop_awww.sh`、`simple_wallpaper.sh` 代理到 `bgpickerctl` 或管理简洁模式。

本地验证命令：

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash -n bgpicker.sh picker_action.sh wallpaper_engine.sh simple_wallpaper.sh prepare_wallpaper.sh switch_wallpaper.sh backdrop_awww.sh backdrop_parallax.sh
python3 -m unittest discover -s tests -v
QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/qml
```

详细版本演进与各阶段优化记录见 `UPDATES.md`。

## 来源

轮播基于 [motor-dev/wallpaperCarousel](https://github.com/motor-dev/wallpaperCarousel)。视差交互参考 [StatIndet/quickshell](https://github.com/StatIndet/quickshell)。前端选择器使用 [Quickshell](https://quickshell.org/)，合成器交互基于 [niri IPC](https://niri-wm.github.io/niri/IPC.html) 与 `zwlr-layer-shell-v1`。
