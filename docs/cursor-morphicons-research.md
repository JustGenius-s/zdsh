# Cursor 图标与 morphicons 迁移研究

研究日期：2026-10-23。目标是以 Cursor 的图形、尺寸和语义状态为参照，建立 zdsh 的统一图标合集，并让会变化的应用图标通过 MorphIcon 过渡。实现位于 `crates/gpui_icons`，资源由 `script/generate_gpui_icons.py` 生成；交互与视觉待手动验收，Roadmap 暂不勾选。

结论：可在应用层实现，不需要修改 GPUI 核心。Cursor 的主图标来自自有字体，字体中的线框外观也是填充轮廓。morphicons 2.0 新增的 region 管线可以处理这类轮廓和孔洞，因此不必从字体反推描边中心线。原始字形与动画轮廓应分别保留，静止时画原图，变化时运行几何插值。

## 证据范围

本机 Cursor **3.27.6**，commit `f7119520544accb8dcf6d27119b9f1963d8ee5a0`，构建日期 `2026-10-21T09:08:09.275Z`。安装根目录为 `/Applications/Cursor.app/Contents/Resources/app`。

使用 REA 6.4.0 CLI 分析安装包的原始 JavaScript；另用静态 CSS、字体表及解码后的字面数据核对资源。没有启动 Cursor 或 zdsh，没有执行目标 JavaScript。开源 morphicons 和 zdsh 本身使用普通源码分析。

| 编号 | REA Evidence ID | 分析载体与结果 |
| --- | --- | --- |
| I | `ev_3b25df92546a7aa87beff446191075fca8cd9522e8acdf078088c98c37cc6f9f` | 原样复制的 `molten/dist/renderer/assets/icon-liiEau9V.js`；1 文件，24,559 字节，4,008 AST 节点，解析失败 0 |
| U | `ev_52bd27f4c68f44b8cdf189c5694c61e754dc25a998c7781f3526e1fd759da72c` | 从 Glass bundle 精确截取的 7 个 UI 载体；30,554 字节，10,146 AST 节点，解析失败 0 |

REA 的 application coverage 只在上述载体内 complete；外部导入及动态绑定未完全解析，I/U 分别保留 644/1,774 个 semantic unknown。它们不是整包行为覆盖，也不证明某个分支实际运行。

| 原始资源 | SHA-256 |
| --- | --- |
| `out/vs/workbench/workbench.glass.main.js` | `08c3f6ca873f1f28d0e886e3538684bd32a7ac61529d223db14069ef7b693c85` |
| `molten/dist/renderer/assets/icon-liiEau9V.js` | `d89d0ca304b046208f853e9ef4c1b43a540c6db8902aee72264089f04f51186b` |
| `out/media/cursor-icons-16.woff2` | `420c80f80c2e1f094b91ae993799c93dcba4afc7372a0ccfe8a29d53c64593a7` |

U 的字节范围如下，均为 Glass JS 中从 0 开始、结束位置不包含在内的 UTF-8 字节偏移。为保持原文，载体有时包含相邻的初始化代码，没有改写表达式或补造导入。

| 主要组件 | 原始字节范围 |
| --- | --- |
| `AgentStatusIcon.js` | 12535634–12543823 |
| `PromptInputSubmitButton.js` | 14780141–14784921 |
| `TranscriptCardHeaderExpand.js` | 12637884–12639660 |
| `ToggleIconButton.js` | 5894282–5897824 |
| `useCopyToClipboard.js` | 15039052–15039500 |
| `CopyableLabel.js` | 15200502–15209304 |
| `IconButton.js` | 10310501–10313518 |

完整 Evidence、载体、资源映射和字体统计保存在本次临时调查目录 `/tmp/zdsh-morphicons-20261023/`；开源库固定版本源码在 `/private/tmp/zdsh-morphicons-source-20261023/`。这些临时目录不是永久仓库归档，复核应结合上面的版本、原始位置和摘要。

## Cursor 图标合集

I 的图标名称经过前缀压缩，码点经过 base36 差分压缩；Glass JS 中解码器和数据位于字节 4516771–4528848。解码静态字面数据，并与字体 cmap 交叉核对后得到：

| 项目 | 数量或行为 |
| --- | --- |
| 公开名称 | 818，包含别名 |
| outline | 682 个不同字形 |
| 显式 filled | 404 个名称，326 个不同字形 |
| filled 回退 | 其余 414 个名称复用 outline |
| 两种 variant 合计 | 1,008 个不同字形，均能在字体中找到 |
| 字体完整容量 | 1,647 glyph slots；不能当作公开图标数 |

`filled` 是独立字形，不是运行时给 outline 设置 fill。有些会变成实心，有些只是线条加粗，不能用统一的填充操作生成。别名也不能全部丢弃：方向镜像按名称判断，`redo` 与同字形的 `refresh` 可以有不同 RTL 行为。

字体为 TrueType `glyf` 闭合轮廓，保留二次曲线，没有 SVG viewBox、stroke centerline、stroke width、cap/join 或动画对应点。单位为 300/em，hhea ascent/descent 为 300/0。682 个 outline 字形的 advance 都是 300；合并后的 1,008 字形中，`cloud-laptop-filled` U+F5B3 的 advance 是 318，其余是 300。部分字形还有负 sidebearing，不能逐图按紧包围盒重新居中拉满。

两个字体副本字节相同：`out/media/cursor-icons-16.woff2` 与 `molten/dist/renderer/assets/cursor-icons-16-QUmxS7q3.woff2`。

### 尺寸与样式

| 部分 | 静态值 |
| --- | --- |
| 默认图标尺寸 | `sm` |
| xs / sm / base / lg / xl / 2xl | .625 / .75 / .875 / 1 / 1.25 / 1.5rem |
| root=16px 时的名义尺寸 | 10 / 12 / 14 / 16 / 20 / 24px；未测运行时 root |
| 字重、行高 | 400、1 |
| 数值尺寸 | 显式数字按 px；也支持 CSS var |
| 禁用、隐藏 | opacity .4、visibility hidden |
| 颜色 | 默认继承；显式颜色走语义 token |
| RTL | 名称含独立 left/right，或指定的 history、quote、redo 等；当前 77 个名称匹配 |

根 token 中 primary 使用 `--cursor-base`，secondary/tertiary/quaternary 分别为其 66%/52%/28% 与透明色混合。Glass 和高对比模式还会覆盖这些值，因此迁移应继续绑定 zdsh 主题语义色，不能把某张截图的 RGB 写入所有图标。

Cursor 通用 Icon、Codicon、文件图标和品牌图标是不同路径。通用 Icon 没有自动 Codicon 回退；文件图标默认 Seti，也支持 cursor/cursor-mixed；品牌可以来自独立 SVG 或图片。zdsh 的文件图标主题、供应商图片和多色品牌应保留各自的解析器，动态控件与状态标记再使用统一 MorphIcon。

### 状态变化的实际实现

下面是 U 中能够追到的分支及相关 CSS，不把 CSS 名称当作算法证据：

| 场景 | Cursor 静态实现 | 对 zdsh 的意义 |
| --- | --- | --- |
| 复制成功 | `CopyableLabel` 在 copy/check 名称间切换；通用复制 hook 成功后保持 2 秒 | 保留成功时机，图形切换改走 MorphIcon |
| 展开收起 | `chevron-right` 旋转 90°；slow=.2s、out-strong；减少动态效果时取消 transition | 方向变化也走统一入口，保留刚性旋转效果 |
| ToggleIconButton | 两个重叠图层；opacity 0→1、scale .97→1、blur 2px→0，fast=.1s | 这是叠层过渡，不是路径插值；本次按用户要求改成几何变形 |
| 输入框提交 | 无输入且运行中显示 filled stop-circle；其他情况显示 filled arrow-up；save 为 check | 发送、停止、保存与尺寸都要作为完整状态，不能只做二元切换 |
| Agent 状态 | draft 为 circle-dashed，进行中为环形进度，需要关注为 exclamation-circle，完成为 check-circle，错误另有组件 | 进度、状态颜色和图形过渡需要共同保留 |
| 通用 loading/spinner | 专门的旋转/扫动分支，含减少动态效果及页面可见性处理 | 进入、退出状态可 morph，稳定运行阶段继续表达进度 |

Toggle 和 chevron 的规则在 `molten/dist/renderer/assets/boot-BVZrksvb.css` 中：`.ui-1uz9zvi` 指定叠层过渡属性，`.ui-1iffjtl` 是 rotate(90deg)，`.ui-ib6ng1` 在 reduced-motion 容器内禁用过渡。

整包 15,322 个指定文本文件的字面搜索没有找到 `morphicons`。`ui-icon-morph-in` 只出现于两份 CSS 定义，内容为 opacity 0→1、scale .94→1，未找到字面调用。由此只能说**尚无证据证明 Cursor 使用 morphicons**；本次是参照 Cursor 图形，再移植独立开源库的算法。

## morphicons 2.0 的迁移内容

固定版本为 [v2.0.0 的 commit 74118e7](https://github.com/guillermolg00/morphicons/tree/74118e766f2e2f0026df334163fc1943eb01f6ab)，发布于 2026-10-15。算法采用 [MIT 许可](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/LICENSE)，移植时保留版权与许可文本。当前 main 相对该版本的变化仅在 website，本方案固定到 tag 对应 commit。

### 描边管线

SVG 的 M/L/H/V/C/S/Q/T/A/Z 和基本图形先转为 cubic chain；二次曲线精确升阶，椭圆弧按不超过 90° 分段。默认每条子路径采样 64 点，使用弧长采样及角点锚定，不是按曲线参数等分。弧长采用 8 点 Gauss–Legendre 积分，反求参数采用有二分保护的 Newton；切线夹角超过 22.5° 的连接处保留角点。[解析与规范化](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/core/normalize.ts)、[采样](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/core/resample.ts)

子路径匹配代价为质心距离加 `0.35 × 长度差`。相同数量且不超过 8 条时穷举配对，较大集合改贪心；数量不同时使用满射，让部件能够分裂或合并。点对应还搜索反向、闭环起点，通过 Procrustes 求旋转与缩放。若整体残差小于 .005，各部件共享整体变换，避免箭头旋转时尖端向轴心塌陷。[配对与整体变换](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/core/plan.ts)

主要插值形式为：

```text
P(t) = lerp(cA, cB, t) + σ^t · R(tθ) · ((1−t)·aC + t·bT)
```

即平移线性插值、旋转角线性插值、缩放按对数插值、剩余形变线性插值；可接受 spring 的 t>1 超调。输出数组预分配，数值插值阶段每帧不分配。[插值](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/core/interpolate.ts)

### 填充与孔洞管线

Cursor 字形的 outline 与 filled 两种 variant 都走该管线。输入必须明确标记为 fill；裸 `d` 字符串在上游默认按 stroke 处理。

上游先将可见表面转成 signed distance field，合并重叠区域，再用 marching squares 提取轮廓。默认网格间距 .125，曲线 flatten 容差 .004，过滤面积不超过 .01 的小轮廓；再简化并恢复尖角。输出采用 nonzero，区分外环、孔洞及归属。[region normalizer](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/region/index.ts)

Region planner 与 stroke planner 不同，必须一并移植：

- 固定轮廓绕行方向；外环只与外环配对，带孔洞的外环不可复制。
- 孔洞只在配对的外环内部匹配，bbox IoU 阈值 .25；未配对孔洞分半程缩没或长出。
- 检查孔洞越界与相交，必要时拆成消失和出现，不能简单把所有 path 按索引配对。
- 默认 64 点，轮廓误差超过 .3 时升级到 128 点；外环旋转惩罚 .3，并尝试减少中间形状自交。

[region planner](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/region/plan.ts)

已规范化的单路径可通过 `data-region` 契约跳过 SDF。但字体复合轮廓可能重叠，不能仅因为来自字体就跳过规范化。上游自适应采样默认面对密集折线；直接输入少量曲线段还需检查曲线内部误差。

上游 fill 的静止结果默认也是 SDF 生成的轮廓，可能改变原字形。因此 zdsh 分两套表示：**原始 SVG 用于静止及结束，规范化 region 只用于动画**。统一保留字体 em、advance、baseline 和坐标翻转，不能逐图 fit 紧包围盒。这样能保住几何终态；字体 hinting 和栅格化方式仍可能产生像素差异。

实现与上游一致，region 不预生成：`glyphs.bin` 每个字形只存静态 SVG 和对应的 SVG 节点（Cursor 字形为单个 nonzero path，fallback 为继承过绘制属性的原始元素），`gpui_icons::region` 在字形首次参与动画时解析节点、运行 SDF 并缓存采样结果。与上游不同的是，半透明图层保留在过渡剪影里，静止时由原 SVG 恢复各层透明度。删除 TS 预生成前，用 Rust 管线对全部 1,047 个字形与原 TS 结果对照：静态 SVG 全部字节一致，1,043 个 region 顶点完全相同，其余最大偏差 .0028 单位；release 下每个字形平均 1.3ms，最慢 2.5ms。dev 配置对该 crate 开 opt-level 3，避免调试构建首次动画时明显卡顿。

### Spring 与中途切换

Spring 方程为 `ẍ = k(1−x) − c·ẋ`，采用先更新速度、再更新位置的 semi-implicit Euler。每帧间隔限制为最多 .1 秒，子步数为 `clamp(ceil(dt/(1/240)), 1, 16)`。停止阈值为 `|1−x|<.001` 且 `|v|<.02`。[spring](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/core/spring.ts)

| 预设 | stiffness / damping |
| --- | --- |
| smooth | 170 / 26 |
| snappy，默认 | 420 / 30 |
| bouncy | 300 / 14 |

中途切换目标时，从当前已画出的点和孔洞状态建立新 plan；进度归零，速度保留并限制到 ±14。尚未画第一帧就再次切换时，先计算旧 plan 的 t=0，避免从空缓冲起步。该机制保持位置连续，不保证每个几何点严格的速度连续。结束时停止回调并恢复目标原图；相同目标不重新开动画。[状态机](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/src/dom/index.ts)

上游有 sample/canonical/pair-plan 缓存；region 内容缓存为 64 项 LRU。浏览器 DOM、rAF、延迟模块加载不需要迁移，调度和销毁语义需要保留。上游 reduced motion 默认 never，且不会主动终止已在飞行的动画；zdsh 应接现有系统设置，在减少动态效果时直接切终态，并停止已有循环。

算法处理单色几何，不等于完整 SVG 合成器。多色图层、mask/clipPath、透明度合成及品牌图片不能直接当作单轮廓。上游也记录了个别图标对中间帧自交的问题，不能承诺任意图标对都没有拓扑瑕疵。[模型边界与上游记录](https://github.com/guillermolg00/morphicons/blob/74118e766f2e2f0026df334163fc1943eb01f6ab/docs/adr/0004-regions-are-canon-normalizer-is-lazy.md)

## zdsh 接入方案

源码基线为 `6670d034d6`。当前有旧 `icons::IconName` 和 Kit 的图标体系，应用资源又优先于 Kit fallback。仅替换组件或同名枚举，不会自动换成 Cursor 图形。

| 当前入口 | 已确认的问题或可复用能力 |
| --- | --- |
| `crates/icons/src/icons.rs`、`crates/assets/src/assets.rs` | 旧 SVG 路径与 Kit fallback 共存，需要明确新集合的命名空间及映射 |
| `crates/ui/src/components/icon.rs` | 内置单色、外部 SVG 与图片共存，不能整体按单色替换 |
| `crates/workspace/src/panel_header.rs` 的 KitIconButton | 公共工具栏接入点 |
| `crates/agent_ui/src/conversation_view/thread_view/composer.rs` 的 PillButton | 图标槽当前接受枚举，需要允许 MorphIcon 内容；发送和停止的宿主 ID 需要统一 |
| `crates/agent_ui/src/conversation_view/thread_view/transcript.rs` | kit_icon、复制反馈、已有 Kit 动画使用，可承接公共 API |
| GPUI Kit 0.7.3 的 motion 与 theme motion | 已有 keyed transition/spring、尺寸和减少动态效果能力；不能用普通 tween 代替本次几何算法 |
| `crates/gpui/src/path_builder.rs`、`elements/canvas.rs`、`window.rs` | 已支持 Q/C、fill rule 和路径绘制，可以原生画轮廓，无需修改 GPUI 核心 |

旧 IconSize 与 Kit Size 的命名对应数值不同，旧 SVG 与 Lucide 的 viewBox/线重也不同。新入口需要统一光学尺寸与颜色，尺寸随现有 UI font 缩放；不能简单把所有枚举设成同一个像素值。

Cursor 字形几乎铺满 em，Cursor 界面靠更小的尺寸补偿：静态检索中 `sm`（12px）出现 1163 次，`lg` 239 次，`xs` 212 次，`base` 150 次，`xl` 仅 32 次；侧栏导航为 14px。zdsh 沿用 Zed/Kit 的画框（默认 16px，Rail 20px），这些画框原本配的是留白更多的图标，直接放入 Cursor 字形会大约三分之一、线条也更重。生成的静态 SVG 与 Cursor 字体一致，视框为 `0 0 24 24`，字形铺满画框；动画轮廓从目录元数据 `display_view_box` 读取同一视框。按 Zed 画框取尺寸的 `ui::Icon` 与 `gpui_icons::Icon` 把字形画成画框的 `BOX_GLYPH_SCALE`（0.8）：16px 画框得到 12.8px，14px 得到 11.2px，20px 得到 16px，布局与点击区域不变；按 Cursor 实测取尺寸的侧栏图标传 `glyph_scale(1.)`。Kit 组件内部的图标不经过这一缩放，按画框原大显示，比原先大四分之一。

建议的组件契约是：稳定宿主 ID、语义图标名称、variant、尺寸、颜色、运动策略。静态和动态使用同一资源注册表；动态宿主持有目标、当前 plan、spring 和绘制缓冲。虚拟列表的 ID 应绑定真实消息/工具/任务 ID，不绑定可变化的索引或图标名称。

原生数值层使用 f64，绘制时转成 GPUI 路径；动画轮廓显式选择 nonzero fill。每帧只更新已分配的点，再绘制路径；不要每帧生成新 SVG 字符串，否则会不断产生 SVG mask/atlas 缓存条目。路径 tessellation 的成本仍需实际验证，不能把“插值不分配”扩展成“整个渲染不分配”。

未显式传 ID 的图标以代码位置为默认 ID。同一调用点在同一父元素下生成的兄弟图标 ID 相同，GPUI 会把前一个刚写入的状态交给后一个，不同字形会互相改目标。MorphIcon 因此记录本帧已绘制的 ID，重复者画静态图标；需要每项都能过渡的列表必须传唯一 ID。

### Cursor 缺失的字形

Cursor 字体没有的应用图标不再沿用旧 Zed 或 Lucide 的原图，而是按 Cursor 风格补画，放在 `crates/gpui_icons/assets/drawn/`，由 `drawn-sources.json` 声明名称和对应的旧资源路径。风格参数取自 Cursor 字形：24 单位网格，1.92 中心线描边，圆头圆角，圆形外沿 0.56、方形 1.28..22.72 的关键线，斜线从 (1.52, 1.52) 到 (22.48, 22.48)，底图在斜线两侧留 1.12 空隙。部分几何取自 Lucide 后按 Cursor 关键线逐个缩放，`derived_from` 记录来源，许可见 `drawn/LICENSE-LUCIDE`。

映射以 Cursor 自己在同类界面里的用法为准，而不是按名字找近义词：在 Cursor 打包代码里静态检索图标名与相邻 label，例如 Files 入口用 `file`，设置用 `settings-gear`，worktree 用 `fork`，文件状态用 `diff-added`/`diff-removed`/`diff-modified`，还原窗口用 `squares`，`diagram` 只用于 Mermaid。Cursor 没有对应形状时（如树状列表 `list-tree`）按上面的风格补画。

能用现成字形表达的直接映射：Shift 用 `arrow-block-up`，Control 用 `chevron-up`。Delete 键改用补画的 ⌦，不再映射到 `trash`。没有代码引用的 13 个图标（Copilot 状态、`debug_ignore_breakpoints` 和 9 个 Kit 默认图标）不补画，继续使用原资源，并记在 `unmapped-icons.json`。

### 必须覆盖的变化场景

| 范围 | 状态及注意事项 |
| --- | --- |
| 全局复制、Markdown 代码块、Agent 消息 | copy ↔ check；保持原有成功时机与反馈颜色 |
| Agent 输入、二次编辑 | send / stop / save / queued 或 loading；运行时有新输入仍可能是发送，不能丢失业务状态 |
| 会话、工具、思考、终端命令块及列表展开项 | chevron 的方向变化；同一宿主持续存在 |
| Agent、子代理、计划项 | idle/running/approval/success/error/cancel；颜色、进度环和旋转不丢失 |
| 终端与 Background Task | running/success/failure/unknown；右侧 Tab 状态与内容区一致 |
| Debugger | pause ↔ continue，保留快捷键、tooltip 和动作绑定 |
| Rail、侧栏、工具栏及通用控件 | outline/filled、视图切换、固定/取消、显隐、播放/停止等实际会换图形的状态 |
| 文件树与文件 Tab | 保留用户图标主题；展开箭头及应用状态徽标接统一动画；文件资源本身不强制变成单色 |

以上是源码已识别的主要接入范围，不宣称已逐个完成迁移。实施时还需扫描剩余条件 `IconName`、动态 `.path()`、旋转及 busy 分支，将所有应用内图形状态切换纳入同一入口，记录外部图片/文件主题等保留项。只有这一轮清点完成，才可标记“所有需要变化的图标”完成。

### 实施顺序与验收

1. 建立资源合集：名称、别名、variant 回退、字体度量、canonical 曲线和预处理 region；建立旧语义名称的显式映射。应用 App 图标重设计仍是 Roadmap 的独立事项。
2. 移植纯 Rust 几何层：解析/规范化、采样、配对、Procrustes、region planner 和 spring；region 按字形懒计算并缓存，同一字形只运行一次 SDF。所有图标对都按 region 过渡，不移植上游两端皆为描边时使用的 stroke planner。
3. 实现应用层 MorphIcon 和可插入 Kit 按钮的图标槽：稳定 ID、中途切换、终态恢复、主题与字号变化、减少动态效果、可见性及销毁。
4. 按上表迁移全部应用状态，并清点残留。图标只变颜色或透明度时不需要重建几何 plan；静止图标不持续请求帧。

验收应包括连续快速切换、复制后立即切消息、运行中输入新消息、outline↔filled 的孔洞、大小不同的 send/stop、虚拟列表滚出后返回、UI 字号/主题切换，以及减少动态效果。文件主题、品牌图片和无障碍标签保持正常。

遵照用户偏好，不新增测试、不自行启动应用。实现阶段运行 `cargo check --locked -p zed --bin zed` 及必要的 `./script/clippy`；交互和视觉由用户手动验证，编译与静态对照通过不等于视觉验收通过。
