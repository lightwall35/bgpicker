# BgPicker：壁纸选择、实时 GPU 模糊背景与视差引擎

适用于 **niri**（Wayland）的壁纸工具。`rust` 分支将静态壁纸、动态视频壁纸、视差排版、换图过渡着色器、概览实时模糊，以及按需拉起的纯 Rust 双页倾斜卡片选择器（`bgpicker-picker`）全面整合进同一套 Rust 工作区，面板关闭即退出释放（**0 MiB** 常驻内存/显存）；同时仍保留 `Quickshell` 旧版前端作为可选回退（`BGPICKER_USE_QS=1`），简洁模式则使用两个受管理的 `swaybg` 实例极简运行。

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
./bgpicker_stats.sh [-w [秒数]]        # 一键抓取 bgpicker-daemon 与按需面板的 CPU/PSS/VRAM 占用
```

也可以直接使用编译好的 `bgpickerctl` 客户端（位于 `target/release/bgpickerctl` 或环境变量 `BGPICKERCTL` 指定路径）：

```bash
bgpickerctl status [--json] [--no-start]
bgpickerctl picker [toggle|open|close|next|prev]
bgpickerctl apply-static <图片路径> [--mode awww|parallax] [--keep-backdrop | --backdrop <on|off|keep>]
bgpickerctl apply-video [normal|parallax|toggle] [视频路径] [--keep-backdrop | --backdrop <on|off|keep>]
bgpickerctl mode [awww|parallax|toggle] [--keep-backdrop | --backdrop <on|off|keep>]
bgpickerctl backdrop-parallax [on|off|toggle]
bgpickerctl transition [fade|wipe|wave|grow|outer|random|none]
bgpickerctl engine [status|full|simple|video|toggle]
bgpickerctl reload-settings
bgpickerctl clear-cache [--deep] [--picker] [--disk] [--all]
```

按 `Mod+N`（或 `./bgpicker.sh toggle`）按需拉起原生纯 Rust 选择器面板 `bgpicker-picker`：
- **按需零常驻与自动对齐页面**：关闭或确认换图后进程自动退出，空闲占用 **0 MiB**；打开时自动检测当前引擎状态——正在播视频壁纸时直接定位到动态页，静态壁纸时定位到静态页。
- **丝滑交互与动画**：支持键盘、鼠标滚轮上下切页、鼠标左键左右拖动与惯性轻甩选图、悬停无跳变连续弹簧缩放、长按 1.5 秒高分辨率原图放大预览，以及面板与顶部 `Ctrl+F` 搜索框的可中断平滑打开/关闭过渡动画。
- **缩略图与模式控制**：静态与动态页目录通过 `bgpickerctl catalog` 扫描生成，统一以 `1280×672` JPG 缩略图缓存至 `~/.cache/bgpicker/{image,video}-thumbnails/`；面板底部提供“普通／视差”倾斜开关与 7 种切换效果按钮，切换效果全局共用，普通／视差在静态与动态页各自独立保存。

### 视频解码与实时模糊机制

- **单路解码共享前后景**：`bgpicker-video` 守护线程通过 FFmpeg C FFI（优先自动探测硬件加速与零拷贝，不可用时回退软件解码）解码单路视频并写入三缓冲队列，同时供前景和概览后景使用，不再需要双播放器或 `ffmpeg` 预先转码整段 1080p 模糊视频缓存。
- **GPU Dual-Kawase 模糊与防闪烁缓存**：后景窗口 `bgpicker:backdrop` 通过 3-pass `DualKawaseBlur` 降采样／升采样着色器直接生成模糊图。静态图启用图像空间缓存（按源图计算，最大 1920×1080 约束下的 FBO 链，仅在换图或首帧计算一次），视差平移期间直接裁剪采样，消除移动时的采样闪烁；视频则仅在 `niri` 概览打开期间（以及刚切换视频的前 4 帧预热期）更新模糊金字塔，概览关闭时跳过模糊计算以节省 GPU 资源。
- **切换体验**：切换视频、视频与静态互切，都在同一组 EGL 表面内直接播放 7 种着色器换图动画，并在过渡期间各自冻结旧画面的 `old_mode_blend`、原子提交目标模式，避免静态与视频跨普通/视差互切时旧画面提前硬平移跳变。切换视频时在后台等待新视频首帧就绪（`min_ready_frames: 2`）再开启过渡，过渡完成后释放旧解码流；静态壁纸按原图首帧纹理加载；`scene.pkg` 与网页壁纸不能直接播放。

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

- **构建 `bgpicker-daemon` / `bgpickerctl` / `bgpicker-picker`**：Rust 工具链（`cargo`）、`pkg-config`、C 编译器（`cc`）、FFmpeg 与 DRM 开发库（`libavformat`、`libavcodec`、`libswscale`、`libavutil`、`libdrm`）、`libxkbcommon`。
- **完整／视频运行期**：Wayland 合成器（niri 会话提供 `NIRI_SOCKET`）、EGL / OpenGL ES 运行时、`ImageMagick`（`magick`，静态缩略图与兼容模糊图）、`ffmpeg` CLI（视频缩略图）、`Bash`；原生面板不再依赖常驻 `Quickshell`（如需回退旧版 QML 面板，可设置 `BGPICKER_USE_QS=1` 并安装 `Quickshell`）。
- **简洁模式补充依赖**：`swaybg`、`Python 3`、`util-linux`（`flock`）。

### 编译

```bash
cargo build --release
```

编译产物位于 `target/release/bgpicker-daemon`、`target/release/bgpickerctl` 与 `target/release/bgpicker-picker`。通过 `./bgpicker.sh` 等包装脚本调用时，若未找到预编译二进制也会自动触发一次 release 构建；运行 `./bgpicker.sh start` 会仅拉起守护进程（选择器面板保持按需启动）。

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
| 显示／关闭选择器 | `./bgpicker.sh toggle`（或 `bgpickerctl picker toggle`） |
| 静态／动态页切换 | 鼠标滚轮上／下，或 `PageUp` / `PageDown` |
| 浏览 | `←` / `→`、`H` / `L`，或鼠标左键左右拖动 / 惯性轻甩 |
| 应用／关闭 | `Enter` / 点击卡片；`Escape` / 点击空白遮罩 |
| 搜索文件名 | `Ctrl+F`（支持平滑弹入与打断收起） |
| 首张／末张 | `Home` / `End` |
| 放大预览 | 停留在当前卡片 1.5 秒自动展开高分辨率原图预览 |
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

默认通过原生 `bgpickerctl picker` 控制面板；若设置 `BGPICKER_USE_QS=1` 回退旧版 Quickshell 面板，原有 `qs ipc` 入口仍保持兼容：

```bash
bgpickerctl picker toggle
bgpickerctl picker next
bgpickerctl picker prev
bgpickerctl picker close
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
| `${XDG_RUNTIME_DIR:-/tmp}/bgpicker-picker-${WAYLAND_DISPLAY:-wayland-0}.sock` | 按需选择器单实例 UNIX IPC 套接字（退出即删除） |

常用环境变量：
- `BGPICKER_DAEMON` / `BGPICKERCTL` / `BGPICKER_PICKER`：手动指定 `bgpicker-daemon` / `bgpickerctl` / `bgpicker-picker` 可执行文件路径。
- `BGPICKER_SOCKET` / `BGPICKER_PICKER_SOCKET`：覆盖守护进程或选择器 IPC 套接字路径。
- `BGPICKER_USE_QS=1`：强制回退至旧版 Quickshell 选择器面板。
- `BGPICKER_BLUR_SIZE`（默认 `1920x1080`）、`BGPICKER_BLUR_SIGMA`（默认 `20`）：控制异步兼容离线模糊图尺寸与模糊半径。

## 目录结构与开发验证

- **`crates/bgpicker-core`**：XDG 路径与原子状态读写、配置规范化与随机效果解析、视差/几何数学、`niri` IPC 客户端、守护进程与选择器 JSON IPC 协议、媒体目录扫描与缩略图缓存。
- **`crates/bgpicker-gfx`**：`smithay-client-toolkit` + `wayland-egl` + `glow` 双层渲染器（`bgpicker:wallpaper` / `bgpicker:backdrop`）、换图着色器（`fade`/`wipe`/`wave`/`grow`/`outer`，含跨模式旧画面 `old_mode_blend` 冻结）、图像空间缓存 3-pass `DualKawaseBlur`。
- **`crates/bgpicker-video`**：基于 FFmpeg C FFI（`ffi/ffmpeg_shim.c`）的异步视频解码线程与三缓冲帧池，支持硬件加速探测、帧步调控制、双帧预热与首 4 帧背景预热。
- **`crates/bgpicker-daemon`**：单线程序列化 Actor 状态机、`bgpicker-daemon` 守护进程与 `bgpickerctl` CLI、`inotify` 配置监听与按需选择器拉起。
- **`crates/bgpicker-picker`**：纯 Rust 原生 Wayland 覆盖层面板（`bgpicker:overlay`）：`state.rs` 状态机与弹簧/三次缓动动画、`render.rs` 单通道 SDF 圆角倾斜卡片与控件着色器、`text.rs` 惰性零拷贝 `swash` + `mmap` HiDPI 中英文字形图集、`thumbs.rs` 有界异步 LRU 缩略图与 1.5s 原图放大缓存、`wayland.rs`/`ipc.rs` 单实例事件循环与守护进程桥接。
- **兼容前端与包装脚本**：`shell.qml`、`BgPicker.qml`、`Carousel.qml`、`CarouselMedia.js`、`SkewSelector.qml`、`WallpaperTransitions.js` 保留作为 `BGPICKER_USE_QS=1` 回退前端；`bgpicker.sh`、`bgpicker_stats.sh`、`wallpaper_engine.sh`、`switch_wallpaper.sh`、`picker_action.sh`、`backdrop_parallax.sh`、`backdrop_awww.sh`、`simple_wallpaper.sh` 代理到 `bgpickerctl` 或管理简洁模式。

本地验证命令：

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash -n bgpicker.sh bgpicker_stats.sh picker_action.sh wallpaper_engine.sh simple_wallpaper.sh prepare_wallpaper.sh switch_wallpaper.sh backdrop_awww.sh backdrop_parallax.sh
python3 -m unittest discover -s tests -v
QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/qml
```

详细版本演进与各阶段优化记录见 `UPDATES.md`。

## 未来优化方向

1. **进一步降低内存、显存与功耗占用**
   - **视频真零拷贝 DMA-BUF / EGLImage 管线**：让 VA-API / NVDEC 硬件解码帧直接导出为 EGLImage/GL 纹理，跳过 CPU 拷贝与多缓冲内存占用，降低动态壁纸的 CPU/显存带宽和笔记本续航消耗。
   - **静态壁纸显存精细化管理**：结合目标显示器物理分辨率与视差缩放上限裁剪纹理上传尺寸，并在空闲期自动触发温和缓存收缩，进一步压缩 `bgpicker-daemon` 的常驻 PSS 与 VRAM。
   - **息屏 / 锁屏 / 全屏遮挡自动休眠**：监听合成器窗口遮挡与输出休眠状态，在桌面完全不可见时暂停视频解码和帧回调，减少无意义功耗。
   - **面板冷启动与峰值内存优化**：进一步收敛 `bgpicker-picker` 首屏缩略图解码并发与解码池复用，缩短按需拉起耗时并降低展开期瞬时显存峰值。
2. **新增更多壁纸切换动画与动效曲线**
   - **扩充 GPU 转场着色器库**：在 `bgpicker-gfx` 中增加百叶窗（blinds）、方向推拉/视差滑动（push / slide）、毛玻璃渐融（blur dissolve）、菱形/像素网格展开、噪声场水墨/流体扩散等更多风格化转场效果。
   - **可定制缓动与分场景策略**：支持在配置中自定义转场贝塞尔曲线，以及为“静态↔静态”“静态↔视频”“视频↔视频”分别设定不同过渡效果或时长。
3. **提升面板视觉美观度与交互质感**
   - **卡片质感与景深层次**：为倾斜卡片增加细腻的高光描边、柔和投影、居中卡片微弱流光与更自然的近大远小景深过渡；并可为面板背景提供轻量级实时模糊蒙层。
   - **壁纸动态取色（Material / 自适应强调色）**：自动提取当前壁纸主色调，让底部滑块胶囊、聚焦边框与搜索框高亮随桌面主题自然融合。
   - **更精致的搜索与卡片信息呈现**：支持实时显示搜索命中计数、无匹配空状态动画、拼音/模糊匹配，以及在卡片角落可选显示精简文件名、分辨率或视频标识角标。
   - **面板外观可配置化**：通过 `settings.json` 直接调整卡片尺寸、倾斜角度、圆角、放大倍率、动画时长与强调色，无需重新编译。
4. **多显示器独立壁纸与跨屏模式**
   - 支持按输出名称（如笔记本内屏与外接显示器）独立配置壁纸、视频与视差开关，并探索超宽跨屏拼接壁纸模式。
5. **壁纸收藏、分组与智能自动轮换**
   - 支持在面板内一键收藏/置顶壁纸、按子目录或标签切换图库分组，以及定时自动换图或随日出/日落、浅色/深色模式自动联动切换。
6. **内置轻量设置抽屉与桌面生态钩子**
   - 在面板内增加可折叠的实时调参入口（视差倍率、模糊强度、转场时长所见即所得），并提供换图后自定义 Hook 回调，方便与锁屏、状态栏及配色生成工具无缝联动。

## 来源

轮播灵感基于 [motor-dev/wallpaperCarousel](https://github.com/motor-dev/wallpaperCarousel)。视差交互参考 [StatIndet/quickshell](https://github.com/StatIndet/quickshell)。可选回退前端使用 [Quickshell](https://quickshell.org/)，原生渲染与合成器交互基于 [niri IPC](https://niri-wm.github.io/niri/IPC.html)、`smithay-client-toolkit` 与 `zwlr-layer-shell-v1`。
