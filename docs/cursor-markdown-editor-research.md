# Cursor 可视化 Markdown 文件编辑器研究

研究日期：2026-10-23。目标是普通 Markdown 文件的可视化编辑器，不是聊天消息渲染器，也不是 Plan 编辑器。本文记录静态研究和 zdsh 的实现方案，**不代表功能已经实现**。

当前状态：2026-10-23 按用户决定暂缓实施，保留研究结论和方案，待用户明确恢复后再推进。尚未修改编辑器源码。

## 目标与证据范围

本机 Cursor 为 **3.27.6**，commit `f7119520544accb8dcf6d27119b9f1963d8ee5a0`。

| 原始产物 | SHA-256 |
| --- | --- |
| `/Applications/Cursor.app/Contents/Resources/app/out/vs/workbench/workbench.glass.main.js` | `08c3f6ca873f1f28d0e886e3538684bd32a7ac61529d223db14069ef7b693c85` |
| 同目录 `workbench.glass.main.css` | `3e5611448d55f1716ff5665a5ebbe3eece529ca714cb0c18796a2ce6d0f6737a` |

研究采用 REA 对精确字节切片做离线 JavaScript 分析，结合原产物代码和 CSS 核对。切片的原文件范围、digest、补全语法边界均有记录。切片目录不是整个 Cursor 应用；REA 未恢复所有动态/跨切片调用，因此下文的调用链是人工追踪原代码得出的静态结论，不能冒充运行观察。

完整本地证据位于 `/tmp/zdsh-markdown-editor-20261023/`：

| 范围 | 文件 | REA Evidence |
| --- | --- | --- |
| 入口、切换、文件生命周期 | `flow/findings.json`、`flow/provenance*.json`、`flow/report.txt` | `ev_397c17427182389e9299c6064eed3936570892039b18482b20a7b8e0998267a1`；`ev_0ce81a3f877dc1a0def110a5332e4ae207d1c0019aff0dcd0b40c38000e42d32` |
| 引擎、语法、同步、编辑交互 | `engine/findings.json`、`engine/provenance.json` | `ev_408dd2838dcdf88dbbf740e0e173307554262ae826eda3e3156a7d45f877f643` |
| 布局、排版 | `styles/findings.json`、`styles/provenance.json`、`styles/atomic-styles.json` | `ev_83d7e39d6ea29b71f2bd0da32ce67565717867b1e2f55d3770c814cf3fd50e63` |
| zdsh 现有实现 | `zdsh-audit.json` | 自有源码审计，不属于 REA 产物证据 |

两次入口分析共 46 个 JavaScript 切片、引擎分析 19 个、样式分析 1 个，解析失败数均为 0。引擎部分语义图受输出预算限制；入口分析也保留了未解析语义。补充源码、CSS 数值是单独核对的静态证据，不计入 REA 的 JavaScript 解析覆盖。没有启动或操作 Cursor/zdsh，没有执行目标代码，没有新增或运行测试。

## 用户实际看到的编辑器

### 文件入口与模式

- 顶栏是 **Preview / Source**。Preview 可以直接修改正文，不是只读预览。
- Glass 对普通 `.md` 默认 Preview，并按完整文件 URI 记住用户手动选择。传统 Markdown 编辑器服务的默认值不同，不能混用。
- 指定行列位置打开文件时临时进入 Source，不改用户保存的偏好。手动切换或换文件解除临时覆盖。
- Glass 入口要求 code 类型、扩展名为大小写不敏感的 `.md`，并排除 Cursor Plan URI 和 `/.cursor/plans/` 下的 `.plan.md`。没有证据将传统入口的 `.cursor/.claude/.codex` 目录排除规则套到 Glass 上；`.mdx` 不走此入口。
- 顶栏更多菜单包含保存、放弃修改、Diff View、路径/链接复制和编辑偏好。保存、放弃修改在 dirty 时可用。
- Preview 的光标、滚动、块位置锚点独立恢复，以工作区和 URI 为范围；用户开始操作后取消延迟恢复，防止抢回光标。

### 格式工具栏

格式工具栏随**非空文本选区**出现。默认顺序是：

1. Ask Agent，以及条件允许时的 Ask Sidechat。
2. Bold、Italic、Underline、Strikethrough。
3. Link。
4. Ordered list、Bullet list、Blockquote。
5. Inline code、Code block。

组间有分隔；按钮阻止 pointer-down 导致的选区失焦，选中态来自当前文档格式。链接按钮展开 URL 输入，Enter/确认提交，Escape 取消并返回编辑器；空地址取消链接。

默认工具栏里**没有**标题、表格、图片、公式的插入按钮。不能因为底层有对应节点，就自行补一排常驻按钮并称为对齐。

只读或流式状态不提供可写格式操作；选区恰好是当前查找命中时隐藏这组浮条。Ask Agent 取选区的纯文本，不是 Markdown 源码。

### 文档布局

| 项目 | 静态样式 |
| --- | --- |
| 内容容器 | 居中，最大宽度 800 CSS px |
| 正文 padding | 上 32px，左右 16px，下 64px |
| Frontmatter padding | 上 24px，左右 16px，下 0 |
| 正文 variant | `document` |
| 正文行高 | 1.6，覆盖通用富文本的 1.42 |
| 正文字号 | `--cursor-font-size-lg`；匹配到的默认 token 为 14px |
| H1–H6 字号 | 1.75 / 1.5 / 1.25 / 1.1 / 1 / 1 em |
| 标题行高、字重 | 1.25、600 |
| 段落下间距 | 0.75em |
| 列表左内边距 | 1.5em |
| 行内/块代码字号 | 0.9em |
| 引用 | 3px 左边框，左内边距默认 token 为 12px |
| 表格 | 1px 边框，单元格内边距默认 token 为 8px，圆角 token 为 6px |

这些是产物中的 CSS 值，不是运行时测量。`ui-*` 原子规则在 `molten/dist/renderer/assets/boot-BVZrksvb.css` 中匹配到；当前研究未追踪其运行时注入和主题覆盖。zdsh 应采用相应比例和已有字体设置，不把 14px 当作不可调整的产品字号。

## 编辑引擎与内容能力

实际文件调用链为 Glass Markdown surface → `renderMarkdownEditor` → `MarkdownEditorReact` → `RichTextEditor` → `useRichTextEditor` → Tiptap editor → ProseMirror state/view/transactions。

文件调用方明确设置可编辑状态、`document` variant、100ms 修改回调去抖、分块初始渲染、Frontmatter UI。这些不是聊天输入框的配置。

| 内容 | 已确认的文件行为 | 未确认或限制 |
| --- | --- | --- |
| 正文、标题、引用、分隔线 | 结构化节点；H1–H6；可直接编辑 | 未做输入法和跨块操作的运行验证 |
| 粗体、斜体、删除线、下划线、行内代码 | marks 和格式命令；下划线序列化为 `++text++` | 不应直接等同标准 CommonMark 的所有扩展 |
| 有序、无序列表 | 自定义 item/order 节点，保留部分原始 bullet/start/delimiter | malformed 输入的全量保真未证明 |
| 任务列表 | 支持嵌套；Enter 拆 item，Tab 缩进，Shift-Tab 反缩进；勾选使用事务并保持焦点 | 原生 GPUI 需要实现同等光标/事务行为 |
| 链接 | URL 编辑、自动链接、文件链接展示；保留部分 autolink 原写法 | 不等于所有链接语法都字节级无损 |
| 代码块 | 可编辑文本、语言属性、语法高亮；支持代码编辑器的带语言粘贴 | 文件插件没有启用代码块通用 Tab 缩进；底层默认 false |
| 表格 | header/row/cell；可调列宽，最小 50px；Tab 下个 cell，最后可加行，Shift-Tab 上个 cell | 引擎含增删行列、合并拆分等命令，但不能据此声称都有文件 UI 入口 |
| 图片 | 行内图片，src/alt/title，保留源路径并在渲染时解析相对/远程路径；允许 base64 | 文件调用未启用 figure/caption 或 resize；粘贴上传/落盘未证明 |
| 数学公式 | 块 `$$…$$`、`\[…\]`，行内 `\(…\)`；KaTeX，atom 节点 | 当前链路未证明行内 `$…$` 或公式源码编辑弹窗 |
| Mermaid | Mermaid fence 转换为可编辑节点，预览和源码区并存；Tab/Shift-Tab 处理缩进 | 不能据此承诺聊天中所有图表交互也存在 |
| Frontmatter | 文件首部 YAML 属性面板；正文编辑保留原 prefix；支持改值、增删、重命名、排序、类型转换、复制粘贴 | 数组/scalar/非法 YAML 回到正文；重复 key 有保护；结构变化会重写 YAML |
| Project database | 文件插件有 feature gate | 不是普通 Markdown 基础能力，不纳入无条件对齐 |
| HTML、脚注、MDX、自定义语法 | HTML 经 active schema 转换；未知 token 可能没有对应节点 | 没有通用无损承诺；`.mdx` 不属于这个文件入口 |

代码块的已确认行为还包括：开头/空块 Backspace 清除块格式；末尾连续三次 Enter 退出；末尾 ArrowDown 到块后。代码块输入规则识别三反引号或三波浪号加可选小写语言和空白。这些输入规则与加载、序列化不是同一条路径。

Frontmatter 展示的类型包括字符串、有限数值、布尔、字符串数组、日期和日期时间。可能丢数据的类型转换有确认。属性复制/剪切使用单属性 JSON 和进程内类型缓存，粘贴接受 JSON 对象，不是 YAML 剪贴板转换。

## 文件同步、撤销与恢复

### 同步和保存

1. 优先读取已有 text-file model，否则读文件；分离 Frontmatter 和正文。
2. 用户修改结构化正文，100ms 后序列化为 Markdown，并写回同一 text-file model 的编辑操作。每个按键并不直接写磁盘。
3. 保存前、模式切换前、卸载前显式 flush，避免去抖尾部丢失。
4. dirty、保存、放弃、关闭确认接入文件层。开启自动保存时本地 200ms、远程 1000ms；untitled 不走此自动保存。
5. 文件 UPDATED 事件先 flush；只有 clean 时自动重载，dirty 内容不直接被磁盘事件覆盖。
6. 外部 reload 与用户修改分开，程序性 setMarkdown 不反向产生相同修改回调。

Glass 文件调用显式关闭通用 renderer 的 Monaco 内容订阅，不能将其描述为任意两个编辑视图始终无条件实时双向同步。同 URI 的 rich owner、挂载队列和文件 overlay 还负责避免重复挂载及旧异步结果覆盖新视图。

### 撤销

正文使用 ProseMirror history，默认 100 步、500ms 合组。Frontmatter 有独立的快照历史；外层记录 body/frontmatter 操作顺序，再把 Undo/Redo 路由回相应历史。连续编辑同一文本属性会合组。

**未证明**切换 Source/Preview、销毁重建或外部 reload 后仍完整保留富文本历史。不能把共享文件模型误说成所有视图共享同一 undo 栈。

### 查找和快捷键

Preview 查找有输入、计数、前后命中、关闭；内容修改时刷新命中，关闭时清除装饰。Ctrl/Cmd+A、F 和历史命令按活动编辑模式分派；有富文本选区时，全局 B/I 避让格式操作。

### 大文档

文件调用启用分块初始渲染，按约 12ms 调度预算逐块挂载；用户开始编辑时先处理剩余内容。通用 hook 的阈值为 131072 字符，但文件路径已显式开启。Glass Markdown 文件分类的上限为 50MiB，不是其他 heavy-preview 的 150KiB 阈值。

上述是静态调度策略，不是性能实测。异常解析路径可能跳过片段，不能据此承诺损坏文档也无损。

## Markdown 保真边界

Cursor 使用结构化解析和序列化，加局部源写法保留，并非完整无损 CST：

- 保留部分列表符号、有序列表起始值和分隔符、粗斜体分隔符、分隔线写法。
- 表格内容未变时保留原表格源码；变更后重新生成，保留对齐信息。
- 正文修改尽量保留未修改 Frontmatter 原文和原先尾部换行数量。
- 进入编辑器时记录序列化基线，不因解析规范化立即制造 dirty。
- 实体、换行、HTML 及未识别扩展可能规范化或丢失；没有对所有 Markdown 字节无损的证明。

zdsh 不应复制这些潜在丢失路径。未知块要保留原始来源并允许 Source 编辑；打开文件、切换模式和移动光标均不能制造文件修改。

## zdsh 现状与实现方案

当前相关代码：

- `crates/markdown_preview/src/markdown_preview_view.rs`：预览 Item、真实 source Editor 绑定、任务勾选、保存委托、来源导航。
- `crates/markdown/src/markdown.rs`：布局、选择、复制、文本来源映射；`MarkdownInputHandler` 的写入方法为空，`accepts_text_input=false`。
- `crates/markdown/src/parser.rs`：携带源范围的语法事件。
- `crates/editor/src/markdown_actions.rs`：现有源码格式操作。
- `crates/zed/src/zed/quick_action_bar/preview.rs`：预览入口。

现有预览每 200ms 重新取全文、异步解析；`Markdown::reset` 清空选区。现有源映射用于展示/复制，存在实体、智能标点和生成文本替换，不能直接把显示偏移传入 `Editor::edit` 当作富文本编辑。

锁定的 GPUI Kit 0.7.3 没有可直接替换的 RichTextEditor。Kit 的 Input 有自己的 Rope/UndoManager，TextView 是 Markdown 展示能力；仓库也未集成桌面 WebView。每段放一个独立 Input 会造成独立历史和跨段选区问题。

采用**原生可写 Markdown 投影 + 同一个真实 Buffer**：

1. `language::Buffer` 和 canonical source Editor 是文档、事务、保存、冲突与历史的唯一依据。Preview 和 Source 是同一文件的两种视图。
2. Markdown 渲染层新增可选编辑接口，处理布局、光标、选区和输入法；编辑控制器留在 `markdown_preview`，避免 `markdown` 依赖已反向依赖它的 `editor`。
3. 显示文本与源范围之间使用带版本和锚点的映射，区分可编辑文本、隐藏语法、生成文本和原子节点。跨格式删除、块合并拆分通过语义编辑操作完成，不能直接按显示字符串删源码。
4. 撤销、重做、输入法组合和 Source 修改进入同一事务体系。外部变更映射当前选区，不通过 reset 抹掉光标。
5. 用 Kit 的 Tabs、Tooltip、Popover、Button、Menu、Input 等搭建文件模式切换、选区格式工具栏、链接和属性 UI；正文编辑引擎是新逻辑组件，不伪装成现成 Kit 组件。
6. 未覆盖的语法保持原文，可进入 Source 编辑；不能在保存时静默丢弃。

不选用 Editor 折叠语法标记作为最终方案：它能复用输入法和历史，但统一行高/字号网格不足以实现真正标题、图片、表格和跨块导航。也不把点击段落后显示原始 Markdown 输入框称为完成可视化编辑。

## 实现与验收顺序

以下均为待实现项，研究完成不等于勾选整个 Roadmap 功能：

- [ ] 文档状态：Preview/Source 同一 Item 身份、按文件模式偏好、位置跳转临时 Source、dirty/save/discard/close/autosave/conflict。
- [ ] 可编辑投影：光标、中文输入法、Unicode、跨行/跨格式选区、复制剪切粘贴、事务和 Undo/Redo；未知语法安全保留。
- [ ] 语义编辑：正文/标题/引用/列表/嵌套任务/行内格式/链接/代码；选区工具条、快捷键和焦点保持。
- [ ] 复合内容：表格 cell 导航和结构操作、图片路径、公式及 Mermaid 的源码/预览编辑、Frontmatter 属性编辑。
- [ ] 布局和恢复：800px 内容宽度、排版比例、滚动/光标恢复、查找、大文档更新。

每阶段对现有实际文件进行读写往返核对；新建测试和启动应用仍遵守用户约定，不自行进行。实现后运行 `cargo check --locked -p zed --bin zed`，应用交互由用户验证。只做研究时不运行无关构建。

特别需要人工验证：组合输入途中切换模式、编辑后立即保存/关闭、跨粗体/链接和多段删除、emoji/实体/多字节字符、Source/Preview 反复切换、未保存时外部文件更新、不同窗口打开同一文件、表格 Tab、重启后模式与滚动恢复。

## 未决证据

- 静态链路已覆盖主要文件流程，但未进行运行时视觉和交互对照。
- Glass 的临时 browse-only/迁移只读状态是否传到已挂载 rich DOM，未确认；实现应统一守卫。
- 底层 provider 拒绝保存后，overlay 成功返回值和最终错误 UI 的语义，未确认；zdsh 必须保留真实错误。
- 表格所有结构命令的文件 UI 入口、图片粘贴上传、公式源编辑弹窗、跨应用 HTML/Markdown 粘贴细节，未从当前调用链建立证据，不能宣称已经对齐。
- Source/Preview 重挂载后的撤销连续性、复杂 Markdown/HTML/脚注的全量往返保真没有运行证据。

这些边界是后续实现和用户验证的核对项，不应在完成状态中隐藏。
