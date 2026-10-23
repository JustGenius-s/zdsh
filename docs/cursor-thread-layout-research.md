# Cursor Threads / History 布局研究

研究日期：2026-10-23。目标为本机 Cursor 3.27.6 安装包。本轮范围是 Threads 面板与历史列表的样式、布局和归档筛选，不引入创建 workspace 功能。

## 证据与边界

通过 REA 6.4.0 `analyze-javascript-application` 分析精确截取的惰性 JavaScript 片段，结合安装包 CSS 核对样式。没有启动 Cursor 或 zdsh，没有执行目标代码；动态功能开关、最终计算样式和实际交互仍需运行时验证。

原始文件：`/Applications/Cursor.app/Contents/Resources/app/out/vs/workbench/workbench.glass.main.js`。

SHA-256：`08c3f6ca873f1f28d0e886e3538684bd32a7ac61529d223db14069ef7b693c85`。

| 证据 | REA Evidence ID | 范围 |
| --- | --- | --- |
| S | `ev_0f16f65f7b460f1f95b2a833e971ad88d127ed4e91b16377a724e1b92c26715c` | 侧栏根组件、分组、Sidebar 原语及样式；11 个片段，全部解析成功 |
| H | `ev_d5abbe29c7aeb9f1485af769a3f14b2f736f2440a978e7df6acb932692d111cc` | 历史、搜索 Palette、归档菜单；22 个片段，全部解析成功 |
| F | `ev_7916969c69fac424060fea1a72217f9d2e5b9761418ad24a8a690dc8b9e473e8` | 日期分组、归档状态、菜单常量等补充调用链；15 个片段，全部解析成功 |

完整 Evidence 和逐片段字节范围、摘要保存在本次本地调查目录 `/tmp/zdsh-threads-ui-20261023/`。片段仅追加分号和换行以形成分析载体，未改写目标表达式；外部全局绑定仍未解析。该临时目录不是仓库内永久归档。

S 的关键原始字节范围（结束位置不包含在内）：

| 组件 | 范围 | 作用 |
| --- | --- | --- |
| `APx` | 46217918–46257681 | 当前 Glass 侧栏根组件，分组与列表渲染调用 |
| `UAx` | 46102812–46122961 | 工作区/仓库分组及会话行组合 |
| `mTy` | 46045268–46047991 | 集合标题及分组布局 |
| `Sidebar.stylex.js` | 12452013–12470368 | 通用行、图标、操作和文字样式 |
| `Sidebar.js` | 12491053–12516571 | Sidebar 组件及状态绑定 |
| `i4` | 45954429–45957385 | Glass 对通用 Sidebar 样式的覆盖 |

CSS 来源为 `workbench.glass.main.css` 与 `molten/dist/renderer/assets/boot-BVZrksvb.css`。CSS 是普通静态资源核对，不将其描述为运行时测量。

## 当前 Glass 侧栏

S 中实际侧栏调用会根据分组方式和 `glass_sidebar_unified_group_headers` 开关，选择 section 或 group 布局。因此安装包同时存在两种分组标题，不可单凭旧组件名称推断当前用户启用了哪一种。

可确认的公共布局与 Glass 覆盖：

| 部分 | 安装包中的值或行为 |
| --- | --- |
| 列表外边距 | `--ui-sidebar-inset` → 8px |
| 行内水平边距 | Glass `--ui-sidebar-item-inset` → 6px |
| 行内容间距 | 6px |
| 行间距 | Glass `--ui-sidebar-row-stack-gap` → 2px |
| 分组间距 | Glass `--ui-sidebar-group-gap` → 16px |
| 圆角 | `cursor-radius-lg` → 8px |
| 图标槽 / 图标 | 20px / 14px |
| 操作按钮槽 | 24px |
| 行高度 | 最小 28px；默认上下各 5px，20px 图标槽可把普通行撑到 30px |
| 文字 | base token 默认 13px、行高 18px；普通字重。部分环境可覆盖 token |
| 分组文字 | Glass 覆盖为 base 字号，不是小号粗体 |
| 选中 | `cursor-bg-tertiary`；选中行 hover 沿用选中底色 |
| 未选中 hover | `cursor-bg-quaternary` |
| 操作区 | 相对时间与 hover/键盘操作共用右侧空间 |

普通分组内容的会话图标与组标题图标共用列起点；独立 NestedRows 原语另有 10px 缩进，不能将它自动加到所有会话行。

## 历史、搜索和归档

H/F 区分了当前 Glass 和仍随包发布的旧紧凑历史弹层。

- 当前 Glass 的归档入口是侧栏 Filters 菜单的 **Archived** 勾选项，沿用原列表。普通过滤排除归档项，勾选后筛出归档项；固定/强制保留项还有独立例外逻辑。
- 搜索使用通用 Palette 的 Agents 模式，包含搜索输入、可横向滚动的筛选 Tabs 和普通结果行。
- Agents 空查询按本地日期分成 **Today / Yesterday / Last 7 Days / Last 30 Days / Older**，只渲染非空分组。
- 结果行包含前导状态图标、单行省略标题、可选同行命中摘要和右侧相对时间，不是多行卡片。操作在 hover 或键盘聚焦时显示。
- 旧紧凑历史弹层为 252px 宽，采用 11px 字体、11px 操作图标，以及独立的 Archived 展开组。这些值只适用于旧弹层，不能套到当前侧栏正文。

归档筛选谓词还用精确原始源码片段 `archive-filter-predicate.js` 交叉检查；该片段是普通源码读取，不属于 H/F 的 REA 图。

## zdsh 对齐范围

- Threads 的项目标题改为普通字重和 UI 字号；会话与终端行共用图标列，移除逐行额外缩进。
- 统一紧凑单行布局、2px 行间距、16px 分组间距、8px 圆角和 14px 前导图标。尺寸用 rem 随 zdsh 的 UI font 缩放，保留用户字号，不强制 Cursor 的 13px。
- 标题收缩、省略；右侧时间和操作使用固定槽，hover 与键盘选择都可以访问操作。已选中行 hover 不再覆盖选中底色。
- 历史列表移除项目、分支的额外文字行，完整标题、路径、分支和归档状态保留在 tooltip 中；采用上述五个日期组。
- 历史的分段筛选改为 Kit Filters 菜单中的 Archived 勾选项；未勾选显示未归档，勾选仅显示归档。导入入口也在菜单中。
- 历史页去掉 New Thread 按钮，Search 接入现有 Agents 搜索。Threads 空态去掉 Open Project / Clone Repository 按钮，不新增 Create Workspace。

zdsh 保留已有的项目分组与 History 返回导航，以及历史记录的排序时间来源、会话恢复、归档/删除和运行状态逻辑；没有照搬 Cursor 的云端项目、分组设置全集或搜索索引。历史重开时内部恢复工作区的能力仍保留，这与提供创建 workspace 的产品入口不同。

## 标题栏、分组标题与侧栏底部

2026-10-23 补充，供整体布局重写使用。同一安装包、同一 SHA-256，REA 6.4.0 `analyze-javascript-application`，coverage `complete`。

| 证据 | REA Evidence ID | 范围 |
| --- | --- | --- |
| L | `ev_a52551ed8f1b63c2b6e3cfbb7198d09870689f2a77b968d4b641768d2ca9619a` | 侧栏底部 `RTx`、账户/设置行 `ITx`、头像 `hTx`、`mTy`、`glass-titlebar.react.js`；5 个片段，全部解析成功 |

| 组件 | 范围 | 作用 |
| --- | --- | --- |
| `RTx` | 46027485–46028766 | `glass-sidebar-footer` 容器 |
| `ITx` | 46023385–46027354 | 底部账户触发区与设置按钮 |
| `hTx` | 46018062–46020128 | 账户头像 |
| `mTy` | 46045268–46047991 | 分组拖动浮层（不是标题行） |
| `glass-titlebar` | 34632366–34632538 | 标题栏拖动区 |

Token 取自 `molten/dist/renderer/assets/boot-BVZrksvb.css`：spacing 以 4px 为 1（`-0-5`=2px，`-1-5`=6px，`-2`=8px，`-2-25`=9px，`-2-5`=10px，`-4`=16px）；height `xs/sm/base/lg` = 20/24/28/32px；font-size `xs/sm/base/lg` = 11/12/13/14px，line-height 14/16/18/22px；radius `xs/sm/base/lg` = 2/4/6/8px。

| 部分 | 安装包中的值或行为 |
| --- | --- |
| 标题栏高度 | `--glass-chrome-bar-height` 40px，另有 44px 变体；整条为拖动区 |
| 分组标题 | 字号 base（13px），起始内边距 `--cursor-spacing-2-25` 9px，结束内边距 4px |
| 底部容器 | 纵向排列，子项间距 10px，宽度 100% |
| 账户/设置行 | 横向居中，间距 8px；账户触发区占满剩余宽度并可收缩；设置是 `gear` 图标按钮（size md，图标 base，tertiary 色调），悬停提示带快捷键 |
| 设置按钮右边距 | `item-inset + (accessory 20px − icon 14px)/2 − (24px − 14px)/2` = 4px，使按钮图标与行尾操作列对齐 |
| base 字号覆盖 | `glass-764vg1` 把 base 局部改为 12px，只用于输入行，不影响侧栏 |

截图中侧栏顶部的「Workspaces」标题和筛选/新建按钮是分组标题加操作按钮的组合，按上表分组标题数值处理。侧栏宽度、头像 `xl` 尺寸没有在静态 CSS 中找到固定值，需要运行时测量；zdsh 侧栏按设计稿取 272px。

## 分组图标、筛选菜单与排序

2026-10-23 补充，供侧栏样式对齐使用。同一安装包、同一 SHA-256，REA 6.4.0 `analyze-javascript-application`，11 个片段全部解析，无解析失败、无完整性矛盾。

| 证据 | REA Evidence ID | 范围 |
| --- | --- | --- |
| G | `ev_eaa9073a64003b8f3ee23c0826b39f926975fbafa8353337a51b910f8f3afc3b` | 分组图标 `OAx`/`oEy`、悬停操作 `nEy`、分组 `UAx`、Customize Sidebar 菜单 `mxx`/`hxx`/`cpn` 及选项常量 |

| 片段 | 范围 | 作用 |
| --- | --- | --- |
| `mxx` | 46137170–46149296 | Customize Sidebar 菜单 |
| `hxx` | 46134794–46135314 | 菜单触发按钮（`list-filter`，size md，icon base，tooltip「Customize Sidebar」） |
| `OAx` / `oEy` | 46100892–46101431 | 分组图标 |
| `nEy` | 46099924–46100204 | 分组悬停操作容器 |
| `UAx` | 46102812–46122961 | 分组与会话行 |
| 选项常量 | 46133071–46133405、46134484–46134736 | Grouping / Ordering / Show 选项 |
| 分组默认值 | 25918761–25918836 | 折叠图标、「More」上限 |
| 图标尺寸 | 46099430–46099498 | `NAx` |

| 部分 | 安装包中的值或行为 |
| --- | --- |
| 分组图标 | 默认 `folder`，展开时换成 `folder-open`；tertiary 色；尺寸 `--ui-sidebar-action-icon-size`，默认 `--cursor-spacing-3-25`（13px）；放在 `--ui-sidebar-action-box`（默认 height-sm 24px）的居中槽里 |
| 悬停操作 | 默认 opacity 0、不接收指针；分组悬停或菜单展开（`aria-expanded=true`）时显示 |
| 「More」 | 每组先列 8 项（`CDt=8`），其余由「More」展开 |
| 菜单结构 | 第一节 Grouping、Ordering、Show；第二节标题「Filters」，含 Status、PR、Environment、Source、Archived，带 Reset；第三节 Mark All as Read |
| Grouping | Repository、Workspace、Updated、Status、Environment、Source |
| Ordering | 会话：Updated、Status；分组：Updated、Manual |
| Show | Updated（时间）、Environment、PR，以及 Workspace、Branch、Machine、Source |
| Status 筛选 | Needs Attention、Error、Unread、Working、Draft、Done |

截图中分组图标在行首、与会话行的状态点同列，名称与会话标题同列；静态代码中统一分组标题开关会把图标放到其他位置，以截图为准。图标渲染实际约 14px，可能来自 `--ui-sidebar-action-icon-size` 覆盖，zdsh 取 14px 与行图标一致。

zdsh 的 `gpui_icons` 与 Cursor 字体一样让字形铺满 24 单位的 em（`display_view_box` 为 0, 0, 24, 24），图形约占 23 单位，可见大小约为盒子的 0.96 倍。侧栏按 Cursor 的实测尺寸取盒子，不经过 `BOX_GLYPH_SCALE`：显示 Cursor 截图中约 14.5px 的图标，盒子取 15.2px；6px 的状态点盒子取 6.4px。

说明：本节和上一节的数值是直接读安装包 JS 和 CSS 得到的；REA 只用于确认这些片段原样截取、全部可解析，并记录来源和哈希，它的分析结果本身不包含样式数值。

zdsh 采用：Threads/Files/Terminals 三个区各有图标（Threads 用 `agent`，悬停时换成展开 `chevron-down` / 折叠 `chevron-right`，Files 用 `folder`/`folder-open`，Terminals 用 `terminal-rectangle`），点击标题折叠；去掉标题后的计数；会话、文件、终端标题统一用正文色，时间用 muted 色；头部名称改为 muted 普通字重，并加入 Customize Sidebar 菜单。菜单只保留 zdsh 有数据的项：Ordering（Updated、Status）、Show › Updated、Filters › Status、Mark All as Read；PR、Environment、Source、Archived、Grouping 不适用于单个工作区的侧栏，未加入。选择不持久化。

## 验证

`cargo check --locked -p zed --bin zed`、修改文件的 rustfmt 检查及 `git diff --check` 通过。只运行格式与编译检查，不补测试、不自行启动应用。窄面板、不同 UI 字号、长标题以及归档恢复的视觉交互由用户验证。
