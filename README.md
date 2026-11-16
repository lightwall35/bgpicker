# BgPicker：壁纸选择、模糊背景与可选视差

独立运行的 Quickshell 壁纸工具，适用于 niri。全屏倾斜卡片轮播负责选择图片，模糊背景按需生成并缓存，由独立 awww 实例绘制。

| 模式 | 清晰壁纸 | 模糊背景 |
| --- | --- | --- |
| `awww`（默认） | 原有 awww 切换效果，支持 GIF | 默认使用独立 awww 实例，可选视差 |
| `parallax`（实验） | Quickshell 绘制，跟随焦点窗口列水平移动 | 默认使用独立 awww 实例，可选视差 |

## 安装与启动

依赖：Quickshell ≥ 0.3.0、awww、ImageMagick 7、Python 3、Bash、GNU coreutils、util-linux（`flock`）。可选安装 `swaybg`，在模糊实例无法启动时临时恢复静态背景。视差模式需要 niri 会话提供 `NIRI_SOCKET`。仓库附带编译后的 Qt 着色器，正常使用不需要安装编译工具。

把原图放进本项目 `wallpapers/`。中文、空格文件名均可；不用预先生成模糊图。个人图片默认被 Git 忽略。在 Wayland 会话终端运行：

```bash
./bgpicker.sh start
./bgpicker.sh toggle
```

普通模式需要运行 `awww-daemon`。niri 自启动示例（替换成实际安装路径）：

```kdl
spawn-at-startup "awww-daemon"
spawn-at-startup "qs" "-p" "/home/lightwall/playground/bgpicker" "-n" "-d"
```

niri 并行启动 Quickshell 和 awww。视差模式换图时同步 awww 的启动缓存；恢复视差时，QS 先准备图片，等待 awww 恢复后再交接。当前仅直接同步已验证的 awww 0.12.1 缓存格式；缓存不匹配时，QS 恢复流程会通过 awww 重新加载当前图。缺失的模糊缓存从原图重建。切换脚本自动启动独立的 `bgpicker-backdrop` awww 实例。为 niri 添加以下规则，让模糊实例显示在概览背景：

```kdl
layer-rule {
    match namespace="^awww-daemonbgpicker-backdrop$"
    match namespace="^bgpicker:backdrop$"
    place-within-backdrop true
}
```

从旧版本迁移时，移除旧 swaybg 自启动项；切换脚本成功应用后只结束正在使用旧模糊缓存的 swaybg 实例。

## 使用

| 操作 | 入口 |
| --- | --- |
| 显示／关闭选择器 | `./bgpicker.sh toggle` |
| 浏览 | `←` / `→` 或 `H` / `L` |
| 应用／关闭 | `Enter` / `Escape` |
| 搜索文件名 | `Ctrl+F` |
| 首张／末张 | `Home` / `End` |
| 放大预览 | 停留在卡片上 |
| 普通／视差模式切换 | 选择器底部中央的倾斜分段选项 |
| 选择换图效果 | 底部“切换效果”，普通与视差共用；下次应用图片时生效 |

```bash
./bgpicker.sh mode                 # 显示已保存的模式
./bgpicker.sh mode parallax        # 用当前壁纸启用视差
./bgpicker.sh mode awww            # 恢复普通模式
./bgpicker.sh mode toggle
./bgpicker.sh transition wave      # 保存两种模式共用的换图效果
./bgpicker.sh transition           # 查看效果及参数
./bgpicker.sh backdrop-parallax on  # 启用模糊背景视差
./bgpicker.sh backdrop-parallax off # 恢复普通模糊背景
./bgpicker.sh backdrop-parallax     # 查看开关状态（也支持 toggle）
./switch_wallpaper.sh ./wallpapers/A.png
```

原有选择器 IPC 保持兼容。将 `<project>` 换为项目绝对路径：

```bash
qs ipc -p <project> call wallpaperCarousel toggle
qs ipc -p <project> call wallpaperCarousel cycleNext
qs ipc -p <project> call wallpaperCarousel cyclePrevious
```

`cycleNext`／`cyclePrevious` 打开选择器并高亮图片，Enter 应用。

普通与视差之间切换时，先准备目标画面，再在居中裁切和视差裁切之间平滑过渡；相同壁纸的模糊背景保持显示。默认过渡 500 毫秒，等待动画完成后再交接渲染器。动画只影响模式切换，换图效果仍由下方的 `transition` 参数控制。

开机恢复视差时，也会从 awww 的居中画面平滑交接到 QS 视差位置，使用同一个 `parallax.modeTransitionMs`；设为 `0` 可同时关闭模式切换和开机交接动画。QS 已在显示视差时重新执行恢复不会重复淡入。

模糊背景视差仅通过上面的命令行开关控制，与清晰壁纸的模式独立，默认关闭；开关状态会保存，启动时随壁纸恢复。它使用缓存的模糊图和相同的视差参数与换图效果；同时启用两层视差会增加纹理、图形内存和渲染开销，两层的具体帧时序可能略有差异。启用／关闭使用 `modeTransitionMs` 平滑交接，关闭后恢复独立 awww 模糊背景。需要上方的两条 niri layer 规则，否则模糊视差层不会正确放入概览背景。

## 换图效果（两种模式共用）

| 界面名称 | 配置值 | 效果 |
| --- | --- | --- |
| 淡化 | `fade` | 新旧壁纸淡入淡出 |
| 斜扫 | `wipe` | 按角度扫过画面 |
| 波浪 | `wave` | 波浪边缘扫过画面 |
| 圆形展开 | `grow` | 从指定中心向外展开 |
| 圆形收拢 | `outer` | 从外侧向中心收拢 |
| 随机 | `random` | 每次在前五种效果中随机选一种 |
| 直接 | `none` | 立即更换 |

选择效果只保存偏好，不立即更换图片；确认一张壁纸后使用该效果。切换普通／视差模式不会重置偏好。awww 模式调用其原生过渡，视差模式用 Qt 着色器绘制对应风格，两者的边缘细节可能稍有不同。

类型、时长、位置、角度和波浪参数保存在同一份 `settings.json` 中。界面修改采用原子写入，保留已有的视差和其他设置。随机效果在一次换图开始时解析一次；启动恢复不会重复播放换图效果，视差模式仍会执行上述模式交接动画。

清晰图和模糊背景共用同一次解析出的类型、时长、方向、圆心和波浪参数，随机效果也只抽取一次。两个进程基本同时启动动画，具体帧时序可能略有差异。

视差动画直接采样新旧图片纹理，结束后释放旧图与着色器对象，平时只绘制当前图片。在 Qt 软件渲染或着色器不可用时退回淡化。

## 视差

Quickshell 直接订阅 niri IPC，按每块屏幕的活动工作区和焦点窗口列计算壁纸位置，以 OutCubic 缓动平移图片；静止时没有持续动画或窗口列表轮询。

长图按屏幕高度铺满，横向移动显示不同部分；普通比例图片稍微放大，提供少量移动空间。焦点进入浮动窗口时保留上次平铺列位置，空工作区居中。上下切换工作区时使用对应工作区的位置，不额外做纵向视差。

- 第一版按焦点窗口列变化驱动，尚不与触摸板或 niri 动画逐帧同步。
- 所有屏幕使用同一原图，每屏独立计算位移；尚无每屏独立图库。
- 视差使用 Qt `Image` 静态显示；GIF 动画用普通模式。图片格式支持取决于本机 Qt 插件。
- 视差窗口位于 `Background` 层，namespace 为 `bgpicker:wallpaper`，不接收鼠标、不抢键盘、不占布局空间；不要将它配置为 `place-within-backdrop true`。
- 视差模式把 awww 表面清为透明，awww 可继续运行；切回普通模式时重新应用原图。桌面时钟等 Bottom 层组件可正常显示。

参数位于 `${XDG_CONFIG_HOME:-~/.config}/bgpicker/settings.json`。界面首次选择效果时自动创建文件；也可以首次配置时复制项目中的 `settings.example.json`（已有配置时直接编辑）：

```bash
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/bgpicker"
cp -n settings.example.json "${XDG_CONFIG_HOME:-$HOME/.config}/bgpicker/settings.json"
./bgpicker.sh stop
./bgpicker.sh start
```

参数示例：

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

| 参数 | 默认 | 含义 |
| --- | --- | --- |
| `zoom` | `1.12` | 普通比例图片放大倍率；长图使用自身横向余量 |
| `columnSpan` | `6` | 从第一列移动到图片另一端所需的列数；`0` 按当前列数铺满范围 |
| `durationMs` | `350` | 视差移动时间，毫秒 |
| `modeTransitionMs` | `500` | 普通／视差模式切换时间，毫秒；`0` 为立即切换 |
| `maxTextureSize` | `8192` | 解码图片最大边长，限制超长图资源占用 |

共用的 `transition` 参数：

| 参数 | 默认 | 含义 |
| --- | --- | --- |
| `type` | `fade` | 上表中的配置值 |
| `durationMs` | `800` | 换图动画时间，毫秒；设为 `0` 立即切换 |
| `fps` | `60` | awww 的动画帧率；视差由 Qt 随显示器刷新渲染 |
| `angle` | `45` | 斜扫／波浪方向，度；`0` 从右向左、`90` 从上向下 |
| `position` | `[0.5, 0.5]` | 圆形中心，归一化坐标，左上角为 `[0, 0]` |
| `wave` | `[20, 20]` | 波浪宽度和振幅；两套渲染器的像素尺度可能不同 |

旧配置中的 `parallax.transitionMs` 仍作为时长回退值；新增 `transition.durationMs` 后以共用参数为准。轮播外观在 `BgPicker.qml` 的 `cfg` 中设置。

## 状态与缓存

| 路径 | 内容 |
| --- | --- |
| `wallpapers/` | 本地原图 |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/current_wallpaper` | 上次成功应用的原图 |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/mode` | 模式；不存在时为 `awww` |
| `${XDG_STATE_HOME:-~/.local/state}/bgpicker/backdrop_parallax` | 模糊背景视差开关；不存在时为 `off` |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/blur/` | 按原图内容和参数生成的模糊图 |
| `~/.cache/current_swaybg_img` | 兼容旧自启动配置的模糊图路径 |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/backdrop-awww/` | 模糊实例独立的 awww 缓存，避免与清晰实例争用恢复记录 |
| `${XDG_CACHE_HOME:-~/.cache}/bgpicker/backdrop-awww.log` | 模糊实例日志 |

确认应用才生成模糊图，浏览时不批量生成。缓存跨重启保留，可删除重建；目前无容量上限。原图不修改。

| 环境变量 | 默认 | 含义 |
| --- | --- | --- |
| `BGPICKER_BLUR_SIZE` | `1920x1080` | 模糊图最大尺寸，不放大原图 |
| `BGPICKER_BLUR_SIGMA` | `20` | 缩小后的模糊 sigma |

切换串行执行。模糊实例使用独立 namespace 和缓存；两层开始切换后才保存原图和模式。失败返回错误，选择器显示错误信息，并尽量恢复之前的壁纸。

## 开发

```bash
bash -n bgpicker.sh prepare_wallpaper.sh switch_wallpaper.sh backdrop_awww.sh backdrop_parallax.sh build_shaders.sh
python3 -m unittest discover -s tests -v
QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/qml
/usr/lib/qt6/bin/qmllint -I /usr/lib/qt6/qml shell.qml BgPicker.qml
```

后端和参数测试用临时目录、模拟后端，不改变桌面。着色器像素测试需要在具有 GPU 渲染的 Wayland 会话内运行（会短暂创建测试窗口）：

```bash
/usr/lib/qt6/bin/qmltestrunner -input tests/render
```

修改 `.frag` 后执行 `bash build_shaders.sh` 重新生成 `.qsb`，需要 Qt Shader Tools 的 `qsb`，可通过 `QSB` 环境变量指定位置。

已在 niri 26.04／Quickshell 0.3.2 的单屏会话验证普通与视差互切、焦点列驱动、桌面时钟层级、启动恢复和加载失败取消。多屏逻辑有隔离测试，尚未在多块实际显示器上验证。

主要文件：`Carousel.qml` 负责轮播，`SkewSelector.qml` 负责底部分段选项，`BgPicker.qml` 连接操作和切换脚本；视差组件负责 niri 状态与背景绘制；`WallpaperTransitions.js` 和 `wallpaper_settings.py` 统一效果参数，`shaders/` 包含过渡着色器；`switch_wallpaper.sh` 负责后端和状态保存，`prepare_wallpaper.sh` 负责模糊缓存。

## 来源

轮播基于 [motor-dev/wallpaperCarousel](https://github.com/motor-dev/wallpaperCarousel)。视差交互参考 [StatIndet/quickshell](https://github.com/StatIndet/quickshell)，按本项目结构实现。运行时使用 [Quickshell](https://quickshell.org/)、[niri IPC](https://niri-wm.github.io/niri/IPC.html)、awww。
