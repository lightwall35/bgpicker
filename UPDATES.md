# BgPicker 更新记录与 Beta 优化规划

记录日期：2026-11-25（最初创建于 2026-11-17）  
当前默认分支：`rust`（包含原 `quickshell-standalone`、`beta` 及统一 Rust 守护进程架构演进）

本文档记录项目自 `quickshell-standalone` 以来在面板精简、`beta` 性能调优、GPU 实时 Dual-Kawase 模糊替换，直至 `rust` 分支统一守护进程重写的完整演进历程。

---

## 一、已完成更新与修复

### 1. 面板界面精简

- 修改 `Carousel.qml` 与 `SkewSelector.qml`：
  - 移除顶部的“静态壁纸／动态壁纸”页标题、中部的“向下滚动 · 动态壁纸／向上滚动 · 静态壁纸”滚轮提示。
  - 移除底部控件上方的“壁纸运动／正在应用”和“切换效果”标题文字，当 `SkewSelector.caption` 为空时直接隐藏对应 `Text` 占位。
  - 仅保留分段按钮内的选项文字（`普通`、`视差` 及 7 种切换效果名称），界面更干净紧凑。

### 2. 视频切回静态壁纸的共享过渡与无缝交接

此前只有“静态 → 动态”和“动态 → 动态”具备过渡动画，从动态视频切回静态壁纸时直接停止视频渲染器，缺乏视觉过渡。现已补齐双向共享过渡：

- **`VideoTransitionLayer.qml` 与 `video_shell.qml`**：
  - 新增静态目标预加载与提交接口：`stageStatic`、`readyStatic`、`commitStatic`、`cancelStatic`，并独立控制目标静态层与模糊层的运动混合比例 `seedBlend`。
  - 在执行 `video -> static` 切换效果（`fade`、`wipe`、`wave`、`grow`、`outer`、`random`、`none`）期间，**旧视频保持持续播放**，动画结束后才释放视频解码器，并保持最终静态画面等待后端交接。
- **`video_wallpaper.py`、`wallpaper_engine.sh` 与 `switch_wallpaper.sh`**：
  - 新增 `video_wallpaper.py transition-to-static IMAGE awww|parallax` 命令及 `prepare_static_blur` 辅助函数。
  - `wallpaper_engine.sh` 在从 `video` 切回 `full` 时先执行共享过渡动画，同步 `awww` 启动缓存后再经 `BGPICKER_HANDOFF=true` 调用 `switch_wallpaper.sh` 即时交接（如果目标是静态 `parallax`，先启动 `awww-daemon --no-cache` 并清空表面，避免中心静止画面闪现），等待静态层就绪后再停止 `video_shell.qml`。
- **测试覆盖**：在 `tests/test_video_reuse.py` 中新增 3 个针对 `transition_to_static` 的单元测试（正常提交、加载失败回退取消、渲染器未就绪返回值）。

### 3. 缩略图清晰度提升与双页面零重载常驻缓存

- **修复模糊问题**：此前卡片 `innerImage` 的 `sourceSize` 被设为 `Qt.size(300, 420)`，导致 16:9 横屏图片先按宽度 300 缩放为约 `300×169`，再被拉伸放大到卡片物理高度（2560×1600 @ 1.5x 缩放屏幕下约 630 物理像素）。
  - 在 `video_wallpaper.py` 的 `prepare_thumbnail()` 中将 FFmpeg 缩放上限从 `640×640` 提升至 **`2560×840`**，并在缓存 key 中加入 `bgpicker-video-thumb-v2-2560x840` 前缀以自动刷新旧缩略图（实测 16:9 视频生成 `1493×840`，超宽视频生成 `1991×840`）。
  - 在 `Carousel.qml` 中定义 `thumbnailSourceSize: Qt.size(2560, carousel.itemHeight * 2)`（即 `2560×840`），同步用于 `innerImage`、`cacheWindow` 预缓存及 2 倍分辨率的 `hiResImage` 长按放大预览。
- **修复滚轮切页重新加载问题**：
  - 在 `Carousel.qml` 中将单一的 `_cacheEntries` 拆分为并存的 `_imageCacheEntries` 与 `_videoCacheEntries`，并在 `cacheWindow` 中分别以独立 `Repeater` 常驻预缓存。
  - 新增 `_setCacheList()` 对比 URL 列表，内容未变时不重新赋值，避免切页时销毁并重建图片纹理。现在静态页与动态页之间来回滚轮切换无需重新从磁盘解码。

### 4. 动态壁纸切换时后景模糊层闪烁修复

排查并消除了切换动态壁纸（及静态↔动态切换）时后景 `bgpicker:backdrop` 透出、跳帧或闪黑的四个根因：

- **场景图预热与防透底（`VideoWallpaperSlot.qml`、`VideoTransitionLayer.qml`）**：
  - 槽位就绪要求至少完成 2 帧解码（`frameCount >= 2`）。
  - 预加载视频槽位在 `stage()` 期间保持 `visible: true` 且位于 `z: -1`，让 `VideoOutput` 在 `commit()` 前即完成首次场景图纹理上传。
  - 将过渡着色器从按需动态加载的 `Loader` 改为常驻 `Item`，`shaders/wallpaper-transition.frag.qsb` 在启动时即编译完成；同时令 `beforeTexture.hideSource: false`，确保切换动画开始首帧底层旧画面始终可见，不会漏出后景模糊层。
  - 静态切视频时 `ready()` 会等待底层静态 `seed` 图片完成解码后再 `commit()`。
- **前后景进度对齐（`video_shell.qml`）**：
  - 由于 1080p 30fps 模糊视频解码快于 4K 60fps 清晰前景，此前切换结束后 2 秒同步定时器会因偏差 >300ms 强制 `seek` 模糊层导致跳帧。现于 `display.commit()` 触发过渡前先将 `backLayer.incoming()` 对齐至 `frontLayer.incoming().position`，实测过渡期与切换后偏差稳定在 17～50ms。
  - `backLayer` 所在 `PanelWindow` 仅在 `backLayer.presented && frontLayer.presented` 时显示，防止模糊窗口先于前景映射。
- **模糊视差交接消黑（`switch_wallpaper.sh`）**：
  - 当 `BACKDROP_PARALLAX=on` 且刚拉起 `bgpicker-backdrop` 时，立即将其 `awww` 表面清为全透明 `00000000`，消除切换期间黑色背景闪烁。

### 5. 环境联动（本机 Shell 语言隔离）

- 调整用户本机 `~/.bashrc` 与 `~/.zshrc`（仓库外本机配置）：仅在处于图形会话（`WAYLAND_DISPLAY` 或 `DISPLAY` 非空）或非 `/dev/tty1` 终端时导出 `LANG=C.UTF-8` 与 `LC_ALL=C.UTF-8`，既让日常终端保持英文 CLI 环境，又避免通过 `/dev/tty1` 启动 `niri-session` 时把英文环境变量传染给桌面图形应用（桌面继续保持 `zh_CN.UTF-8`）。

---

## 二、当前状态基线与已知瓶颈

在 `engine=full`、`mode=parallax`、`backdrop_parallax=on` 单屏（2560×1600 @ 1.5x）状态下对运行进程的实测采样：

| 进程 | RSS 驻留内存 | PSS 分摊内存 | 说明 |
| --- | ---: | ---: | --- |
| `qs -p /home/lightwall/playground/bgpicker -n -d` | 1307.5 MiB | 1059.5 MiB | Private_Dirty 达 976.1 MiB |
| `awww-daemon --no-cache` | 33.9 MiB | 23.9 MiB | 视差模式下仅清透明占位 |
| `awww-daemon --namespace bgpicker-backdrop --no-cache` | 33.9 MiB | 23.9 MiB | 模糊视差模式下仅清透明占位 |

---

## 三、`beta` 分支架构与性能优化规划

为在不降低画质、流畅度与现有功能水准的前提下显著降低内存、显存与 CPU 占用，`beta` 分支计划按优先级推进以下改进：

1. **静态壁纸卡片级磁盘缩略图缓存（高优先级，预期降幅 700～800 MiB）**
   - 现状：`wallpapers/` 中存在多张 4K～8K、体积 10～30 MiB 的 PNG（如 5184×2912 的 `Q.png`）。PNG 不支持流式降采样解码，Qt 必须先分配全分辨率画布解压后再缩小，导致 glibc 多线程堆严重膨胀；且当前解码出的 16:9 宽图有一半像素被竖向倾斜卡片裁切浪费。
   - 方案：在 `media_catalog.py`（或统一缩略图缓存服务）中为静态图片也生成适配卡片尺寸（如 `750×630` 左右、保持高画质）的磁盘 JPG 缩略图，仅在 1.5 秒长按放大预览或确认应用壁纸时加载原图并在预览结束时释放。
2. **面板关闭时释放卡片离屏 FBO 与收敛 `cacheBuffer`（高优先级）**
   - 现状：`Carousel.qml` 的 `ListView` 设了 `cacheBuffer: 5000`，且每张卡片同时启用 3 层 `layer.enabled`（圆角遮罩 + `MultiEffect` + 外层透明度）并在面板隐藏时继续保留在显存与内存中。
   - 方案：将卡片 `layer.enabled` 绑定 `&& overlay.visible`，面板关闭时释放全部离屏 FBO，并将 `cacheBuffer` 收敛到合理窗口宽度。
3. **动态壁纸稳定播放期断开逐帧 JS 信号（高优先级）**
   - 现状：`VideoWallpaperSlot.qml` 通过 `Connections` 监听 `output.videoSink` 的 `onVideoFrameChanged`，导致前景 60fps + 后景 30fps 每秒向 QML/JS 主线程派发 90 次 `QVideoFrame` 与属性通知。
   - 方案：首 2 帧解码就绪（`decoded` 为 `true`）后立即关闭该 `Connections`（`enabled: !slot.decoded`），仅在明确需要调试统计帧数时开启，使稳定播放期 QML 主线程帧信号开销降为 0。
4. **进程与 IPC 架构整合**
   - 共享单例 `NiriState`：`shell.qml` 中 `wallpaperBackend` 与 `backdropBackend` 目前各自建立一条独立的 `NIRI_SOCKET` 订阅流并重复执行 `WallpaperMath.reduce`，合并为顶层共享实例。
   - 合并双 Quickshell 进程：将 `video_shell.qml` 渲染层并入主 `shell.qml` 按需加载，取消独立的 `video_wallpaper.py` 常驻 Python 轮询进程与第二个 QtQuick 实例，并允许在同一个进程内直接完成静态与动态视图过渡。
   - 全视差模式按需停用闲置 `awww-daemon`：当清晰前景与模糊后景均由 Quickshell 视差绘制时，释放仅作透明占位的两个 `awww-daemon`（约省 48 MiB PSS），切回 `awww` 模式时再按需拉起。
5. **元数据缓存键与事件驱动目录刷新**
   - 用 `realpath + size + mtime_ns` 替代 `prepare_wallpaper.sh` 和 `video_wallpaper.py prepare_blur` 每次切换时读取数十 MiB 原文件计算全量 `sha256sum` 的开销。
   - 用 QML `FolderListModel` 监听 `dynamic/` 变动替代 `BgPicker.qml` 中面板打开时每 10 秒拉起一次 `python3 media_catalog.py` 子进程的轮询定时器。

---

## 四、`beta` 分支已落地优化与 A/B 基准对比（2026-11-18）

在 `beta` 分支上完成了上述优化的逐项实现、真实桌面压测调优与回归验证：

### 1. 已实施关键改动

1. **静态与视频双端磁盘缩略图缓存（`1280×672`）**
   - `video_wallpaper.py` 新增 `prepare_image_thumbnail()`（`magick -thumbnail 1280x672> -quality 88 -strip` 生成磁盘 JPG 缩略图至 `~/.cache/bgpicker/image-thumbnails/`），并将 `prepare_thumbnail()` 收敛至同样匹配的 `1280×672`（`bgpicker-video-thumb-v3-1280x672`）。
   - `media_catalog.py` 新增 `IMAGE_SUFFIXES`、`build_image_catalog()`（4 线程并发生成）及 `--bundle IMAGE_DIR VIDEO_DIR` 一次打包输出静态与动态目录。
   - `Carousel.qml` 通过 `imageCatalog` 加载磁盘缩略图并设定 `thumbnailSourceSize: Qt.size(1280, Math.ceil(carousel.itemHeight * 1.6))`，避免直接全量解压 `wallpapers/` 内 10～30 MiB PNG；长按 1.5 秒 `hiResImage` 按需加载原图且设 `cache: false`。
2. **面板关闭释放离屏 FBO 与收敛 `cacheBuffer`**
   - `Carousel.qml` 的 3 层卡片 `layer.enabled` 统一绑定 `overlay.visible && ...`，`listView.cacheBuffer` 设为 `overlay.visible ? 1200 : 0`。
3. **动态壁纸稳态零帧信号与防卡顿隔离架构**
   - `VideoWallpaperSlot.qml` 为 `videoSink` 连接增加 `trackFrames`，在 `VideoTransitionLayer.qml` 仅于 `transitionActive || stagedPath !== ""` 期间开启帧计数，稳态播放期逐帧 JS 信号降为 0/s。
   - 经单进程合并实测对比后，发现将 QtMultimedia/FFmpeg 并入主 `shell.qml` 会导致解码器与 CUDA 初始化阻塞 `Carousel` GUI 线程，且退出视频模式后 FFmpeg/CUDA 堆留存约 370 MiB 难以归还系统（`892 MiB PSS` vs 独立进程方案的 `492 MiB PSS`）。因此架构上将视频渲染逻辑抽离为可复用的 `VideoBackend.qml` 与 `VideoWallpaper.qml`，保持 `video_shell.qml` 按需在独立 `qs` 进程中运行，从而实现 0 代码重复、零面板线程阻塞与退出即全额释放内存；同时 `Carousel.qml` 的 `confirmPick()` 将 `wallpaperPicked` 延后至 300ms 确认动画结束关闭遮罩后触发。
4. **共享 `NiriState`、事件驱动刷新与元信息索引**
   - `WallpaperBackend.qml` 与 `shell.qml` 共用顶层单一 `NiriState` 实例订阅 `NIRI_SOCKET`。
   - `BgPicker.qml` 移除 10 秒轮询 `Timer`，改由 `Carousel.qml` 的 `FolderListModel` 监听 `dynamic/` 自动触发 `catalogRefreshRequested`。
   - `prepare_wallpaper.sh`（`blur-meta-v1`）与 `video_wallpaper.py prepare_blur()`（`bgpicker-video-blur-meta-v2`）均引入 `.ref` 元数据索引，命中缓存时跳过全文件 SHA-256 哈希读取。
5. **全视差按需停用闲置 `awww-daemon` 与 UNIX 套接字直检**
   - `switch_wallpaper.sh`、`backdrop_parallax.sh` 与 `wallpaper_engine.sh` 在进入静态全视差或视频模式后自动清理闲置的前景与 `bgpicker-backdrop` 守护进程，切回 `awww` 或关闭模糊视差时再按需拉起；并新增 `awww_live` / `awww_backdrop_live` 直接检查 `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY-awww-daemon*.sock`，消除进程未运行时调用 `awww query`/`kill` 触发的每次约 507ms 超时重试，同时修复了无 `awww-daemon` 时 `--restore` 阻塞报错的边界问题。

### 2. 旧版（`4137fc8`）vs `beta` 全量性能与卡顿基准对比

测试环境：`eDP-2` 2560×1600 @ 165Hz（理想单帧 6.06 ms），`engine=full`、`mode=parallax`、`backdrop_parallax=on`，隔离对比副本挂载同一媒体目录并使用相同 `FrameAnimation` 探针：

| 指标 | 旧版 (`4137fc8`) | `beta` 版 | 变化 |
| --- | ---: | ---: | --- |
| 静态全视差常驻内存 (`qs` + `awww`) | 1184.2 RSS / **930.6 PSS** (3 进程) | 711.0 RSS / **477.5 PSS** (1 进程) | **-453.1 MiB (-48.7%)** |
| 视频全视差运行内存 (`柠檬_1.mp4`) | 2433.2 RSS / **1900.4 PSS** (3 进程) | 2017.7 RSS / **1485.0 PSS** (3 进程) | **-415.4 MiB (-21.9%)** |
| 视频切回静态后常驻内存 | 1241.4 RSS / **961.8 PSS** (3 进程) | 751.6 RSS / **492.0 PSS** (1 进程) | **-469.8 MiB (-48.8%)** |
| 视频稳态播放 CPU 占用（2s 采样） | 41.21% | **34.76%** | **-6.45 个百分点 (-15.7%)** |
| `prepare_wallpaper.sh` 命中 (`Q.png` 30.3 MiB) | 27.1 ms | **9.8 ms** | **-63.8%** |
| `video_wallpaper.py prepare_blur` 命中 (`柠檬_1.mp4`) | 55.8 ms | **23.8 ms** | **-57.3%** |
| 冷启动并恢复 (`startup_restore`) | 3094.5 ms | **1408.4 ms** | **-54.5%** |
| 静态切静态 (`HW2S -> Q.png`) | 779.8 ms | 922.3 ms（含 300ms 确认动画延后） | 实际后端 ~622 ms |
| 静态切视频 (`static -> video1`) | 3430.1 ms | **2851.4 ms** | **-16.9%** |
| 视频切视频 (`video1 -> video2`) | 2540.5 ms | **1931.7 ms** | **-24.0%** |
| 视频切回静态 (`video2 -> static`) | 3201.6 ms | **2184.5 ms** | **-31.8%** |
| 4 组卡片点选确认动画卡顿帧 (`>16.7ms`) | 0 帧 | **0 帧** (p50 5.99~6.05 ms) | 锁死 165Hz |
| 静切视频期后台最大单帧耗时 | 62.73 ms (1 帧 >50ms) | **39.94 ms** (**0 帧 >50ms**) | 峰值显著收敛 |
| 视频播放期开面板滚动 p99 / max | 8.00 ms / 78.69 ms | **6.97 ms / 67.88 ms** | p99 与尖峰同步下降 |

### 3. 回归验证状态

- Shell 语法检查（全部 `.sh` 脚本 `bash -n`）：退出码 0。
- Python 单元测试（`python3 -m unittest discover -s tests -q`）：57 项全部通过。
- 离线 QML 测试（`qmltestrunner -input tests/qml`）：21 项全部通过。
- `qmllint` 检查全部修改及新增 QML 文件：退出码 0。

---

## 五、`beta` 后续架构精简与视差衔接修复（`c180f28`、`c49dcde`）

在 `beta` 分支验证并落地的两项关键结构优化：

1. **单解码器视频分流与 3-pass Dual-Kawase GPU 模糊（`c180f28`，2026-11-19）**
   - 移除 `video_wallpaper.py prepare_blur()` 的 1080p 30fps CRF 18 离线预模糊 MP4 转码及第二套并行 `MediaPlayer`。
   - 新增原生 Qt 插件 `BgPickerVideo`（`native/VideoFrameSplitter.cpp`）和 `VideoMirrorSlot.qml`、`DualKawaseBlur.qml` 与 `shaders/kawase-{down,up}.frag(.qsb)`：单路视频解码后零拷贝向概览层分发帧信号，并利用 3-pass Dual-Kawase 模糊着色器在 GPU 端实时模糊，概览关闭时冻结更新以节省渲染开销。
   - **修复**：切换视频时不再重走耗时数秒的离线 FFmpeg 模糊转码，双端视频内存和解码占用大幅下降。

2. **静态视差切回普通模式（`parallax -> awww`）平滑衔接（`c49dcde`，2026-11-25）**
   - 在 `ParallaxWallpaper.qml` 增加 `modeExiting` 状态，在 `switch_wallpaper.sh` 中等待 `exitMode` 完成并经 `await_mode_transition` 确认后再启动 `awww-daemon` 与重置透明遮罩。
   - 修复了从视差切回 `awww` 时先启动守护进程或过早清屏导致的黑闪／居中画面跳变。

---

## 六、`rust` 分支：统一守护进程与 EGL/FFmpeg 渲染重写（`e81555f`，2026-11-25）

为彻底消除多进程 QtQuick + `awww-daemon` + Python IPC 的显存、内存峰值与跨表面竞争，新增 `rust` 分支，将壁纸与视频引擎重写为 4 个 Rust crate，并完整保留原有 QML 轮播交互体验。

### 1. 四层 Rust Workspace 架构

- **`crates/bgpicker-core`**：统一 XDG 路径与原子持久化（`PersistedState`）、`settings.json` 解析与随机效果解析、niri IPC 工作区／焦点列／概览状态监听（`NiriWatcher`）、视差几何与过渡数学模型、守护进程 UNIX socket JSON 协议与媒体目录/缩略图生成器。
- **`crates/bgpicker-gfx`**：基于 `smithay-client-toolkit` + `wayland-egl` + `glow`（OpenGL ES）直接接管每个输出上的 `bgpicker:wallpaper`（桌面背景层）与 `bgpicker:backdrop`（niri 概览背景层）两块 Wayland layer-shell 表面：
  - 将静态图与视频帧的 `fade`、`wipe`、`wave`、`grow`、`outer` 换图着色器统一进 `LayerPipeline`。
  - 在同一 EGL 表面内插值 `mode_blend` 完成 `awww`（居中填充）与 `parallax`（列视差）的平滑切换，彻底消除外部 `awww` 进程的表面创建与 Z 序竞争。
  - **图像空间缓存 Dual-Kawase 模糊（`DualKawaseBlur`）**：对静态图以源图像空间（上限 `1920×1080` 比例缩放）离线化算一遍 3-pass FBO 金字塔并在内存缓存在当前贴图周期；视差平移时按裁剪坐标采样已模糊纹理，消除了屏幕空间实时采样随窗口列平移产生的边缘闪烁问题。对视频帧仅在 `overview_open` 为真或新视频首 4 帧预热期刷新模糊链，概览关闭时节省 GPU 开销。
- **`crates/bgpicker-video`**：通过 FFmpeg C FFI（`crates/bgpicker-video/ffi/ffmpeg_shim.c`）与后台解码线程（`worker.rs`）结合三缓冲队列直接向渲染器推送视频帧，支持自动探测硬件加速并安全回退软解、帧步调控制、新旧流无缝过渡以及后台预热。
- **`crates/bgpicker-daemon`**：序列化状态机 `DaemonActor`、`bgpicker-daemon`、`bgpickerctl` 客户端与 `inotify` 配置监听：
  - `simple` 模式仍按极简思路使用两个 `swaybg`（本地副本 `swaybg-front` 配合 `bgpick-fg` namespace + 概览 `^wallpaper$`），进入时自动退出 `qs` 与 `bgpicker-daemon`；退出 `simple` 模式时先由守护进程提交首帧再安全撤下 `swaybg`。
  - 完整／视频模式下，支持前／后景视差默认同步（`BackdropOverride::Auto`）与独立覆盖控制（`--keep-backdrop`、`--backdrop <on|off|keep>`、`backdrop-parallax [on|off|toggle]`）。
  - **动画帧时间锚点修复**：`render_due()` 在表面空闲期不累积 `last_tick`，切换或列移动触发首帧动画前按当前时间重置基准，防止空闲时长被计入第 0 帧导致静态壁纸视差跳变。
  - 保持后台异步生成 `~/.cache/bgpicker/blur/*` 与 `~/.cache/current_swaybg_img`，兼容锁屏等外部组件。

### 2. 前端保留与包装脚本兼容

- 完整保留 `Carousel.qml`、`CarouselMedia.js`、`SkewSelector.qml` 的外观、手势与 165Hz 动画逻辑；精简 `BgPicker.qml` 与 `shell.qml`，通过 `bgpickerctl` 与守护进程通信。
- 保留 `bgpicker.sh`、`wallpaper_engine.sh`、`switch_wallpaper.sh`、`picker_action.sh`、`backdrop_parallax.sh`、`backdrop_awww.sh` 对外命令行接口并转发到 `bgpickerctl`；`simple_wallpaper.sh` 继续服务简洁双 `swaybg` 模式。

### 3. `rust` 分支验证状态

- `cargo clippy --workspace --all-targets -- -D warnings`：零警告通过。
- `cargo test --workspace`：57 项全部通过（覆盖 `bgpicker-core`、`bgpicker-gfx`、`bgpicker-video`、`bgpicker-daemon`，含后续新增的 `clear-cache` 与 `trim_idle` 测试）。
- `cargo build --release`：构建成功（`target/release/bgpicker-daemon`、`target/release/bgpickerctl`）。
- Shell 语法检查（`bash -n`）、Python 单元测试（`python3 -m unittest`）、离线 QML 测试（`qmltestrunner -input tests/qml`）均全部通过。
- 新增 `bgpickerctl clear-cache`（及 `./bgpicker.sh clear-cache` / `trim`）与 `./bgpicker_stats.sh`：静态模式下支持按需关闭空闲视频后端、释放 `DualKawaseBlur` 中间 FBO 与 CPU 帧缓冲、调用 `malloc_trim(0)` 回收堆内存，并支持 `--deep`、`--picker`、`--disk` 分级清理与人类可读资源占用快照。

---

## 七、纯 Rust 按需选择器重构与动效升级（2026-11-27）

将常驻 Quickshell 选择器面板重构为纯 Rust 按需 Wayland 覆盖层二进制 **`bgpicker-picker`**（`crates/bgpicker-picker`），实现空闲 **0 MiB** 驻留与完整原生体验，并同步修复跨引擎视差过渡瑕疵：

1. **纯 Rust 原生覆盖层面板（`crates/bgpicker-picker`）**：
   - 基于 `smithay-client-toolkit`（`zwlr_layer_shell_v1` `Overlay` + `wp_fractional_scale_v1` + `wp_viewporter` + `xkbcommon`）+ EGL/GLES 3.0 单通道 SDF 圆角倾斜卡片与底部胶囊渲染器，完整复刻双页倾斜轮播、1.5s 长按原图放大预览、底部普通/视差及 7 种切换效果控件。
   - 采用惰性零拷贝 `swash` + `mmap` HiDPI 中英文字形图集与有界异步 LRU 缩略图缓存，展开期实测约 `44.5 MiB Private / 102.7 MiB PSS / 89 MiB VRAM`，冷启动约 `199 ms`，关闭后完整释放进程与显存回落至 **0 MiB**；`BGPICKER_USE_QS=1` 仍保留旧版 Quickshell 回退路径。
   - `bgpicker-core` 与 `bgpicker-daemon` 新增 `focused_output` 跟踪、`bgpickerctl picker [toggle|open|close|next|prev]` 与单实例按需拉起；`bgpicker.sh`、`wallpaper_engine.sh`、`bgpicker_stats.sh` 全面适配按需面板。
2. **交互打磨与跨引擎体验修复**：
   - 新增鼠标左键左右拖动与速度惯性轻甩选图（含边缘橡皮筋与循环包裹回弹）；引入解析临界阻尼弹簧 `SmoothSpring` 与连续 `scroll_index` 衰减，彻底消除未展开卡片在指针悬停切换时的尺寸跳变。
   - 修复视频壁纸下打开面板默认停在静态页导致点选普通/视差误切回静态图片的问题：面板打开时自动同步 `DaemonStatus.engine`（视频引擎直达视频页，静态引擎停在静态页）。
   - 修复静态与视频壁纸在普通/视差不同模式间互切时的“先硬平移再转场”问题：`bgpicker-gfx` 的 `LayerPipeline` 新增 `old_mode_blend` 冻结过渡期旧画面几何，`RenderCommand::ApplyFrame` 携带 `FrameModeTargets` 原子更新目标模式，彻底消除预跳变。
3. **面板与搜索框开关动画升级**：
   - 面板打开（`240ms OutCubic`）与关闭（`180ms OutCubic`）增加整体卡片空间缩放、微幅上浮/下沉、底部控件柔和滑入滑出，以及确认换图时的全界面平滑溶解，并将时钟锚定在首帧 `configure`；关闭过程支持 `Mod+N` 中途无缝打断反向弹回。
   - 顶部 `Ctrl+F` 搜索框升级为 `220ms` 滑入缩放淡入与 `160ms` 滑出淡出（关闭瞬间停止拦截输入并保留文字快照避免闪出占位提示）。
4. **验证状态**：
   - `cargo clippy --workspace --all-targets -- -D warnings` 零警告通过；`cargo test --workspace` 全工作区测试通过；`cargo build --release`、`bash -n` 及 Python 单元测试（42 项）全部通过，并在 `niri` Wayland 桌面实测生效。
