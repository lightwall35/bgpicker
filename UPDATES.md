# BgPicker 更新记录与 Beta 优化规划

记录日期：2026-11-17  
当前基线分支：`quickshell-standalone`（准备切出 `beta` 分支进行下一阶段优化）

本文档整理了自接手项目以来已经完成并验证的改进，以及针对当前内存、GPU 与进程架构瓶颈制定的 `beta` 分支优化路线图。

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
