# zdsh roadmap

Shipped behavior is [docs/zdsh.md](docs/zdsh.md).

- **Now** — active work. When implemented, update `docs/zdsh.md` and mark the item complete here.
- **Next** — the only queue.
- **Later** — parked.
- **Out** — do not bring back.

- [ ] Outcome. Surface: `crate`. Done when: …

## Now

- [x] 优化 Agent 面板  - 使用 REA 研究 cursor 后 对齐
  - [x] 移除 commit 按钮 - 这个不对齐
  - [x] 修复打开 context/cursor/内容区 面板会把会话区域和会话导航挤到上方的问题
  - [x] 修复 worked for xx 折叠之后最后一个 Think 内容还在外面的问题
  - [x] 修复对话未开始时，context 统计不展示的问题
  - [x] 增加会话输出到下一个用户消息的间距，现在太近了
  - [x] 移除会话输出下面的滚动到用户消息按钮
  - [x] 会话输出下面的复制按钮点击后变成勾勾并弹窗提示复制成功
  - [x] 增加会话 fork 功能
  - [x] 支持 Cursor Debug 模式（复现步骤、队列等待、日志查看、重启恢复及验证流程已对齐）
  - [x] 支持输出内容选中添加到会话
  - [x] 支持模型 token 速率展示，缓存率展示
  - [x] 支持 Ask Question 工具
  - [x] 命令审批工具优化
  - [x] 支持 Composer 预测，Tab 自动接收，https://learn.chatgpt.com/docs/features/composer-predictions
  - [ ] 研究 Cursor 提示词和工具提示词，分析模型在 Cursor 中能输出高质量总结的原因，进行对齐（部分已对齐）
  - [x] 支持 Background Task. Background Task 会新建终端 Tab 执行. 任务完成之后通知 Agent（[REA 与实现记录](docs/cursor-background-task-research.md)：前台转后台、原会话完成队列、查询/停止、状态保存及重启中断恢复）
  - [x] 修复 Agent 输出换行后标点符号在第一个字符的问题，中文比较突出

- [x] 右侧边栏优化
  - [x] 分类项的菜单默认隐藏，鼠标悬浮后展示
  - [x] 右侧边栏增加 tab 状态，Agent Tab 增加 Agent 运行状态，终端面板增加命令执行状态，文件 Tab 支持显示文件图标

- [x] 文件 Panel 优化
  - [x] 没选中文件时隐藏缩进线
  - [x] 项目选择下拉和 分支/worktree 选择下拉更新为 gpui-kit 组件
  - [x] 右键菜单替换为 gpui-kit 组件，增加 Add to chat 选项
  - [x] 支持单文件 Diff 视图
  - [x] 文件 Diff 样式优化

- [x] Thread Panel 和历史记录优化
  - [x] 样式布局对齐 Cursor，不要 “创建 workspace”功能
  - [x] 去掉 New Thread 按钮和 New Terminal 按钮
  - [x] 去掉下拉菜单，不允许在 Thread Panel 切换项目

- [ ] 全局优化
  - [ ] 以 cursor 为准重新创建图标合集，迁移 morphicons 算法
  - [ ] 优化全局选中文本的样式，现在这个配色太古老
  - [ ] 重新设计 App 图标，让 Dev 版本的图标和 Release 版本的图标以及应用名保持一致

## Next

- [ ] 设置菜单优化
- [ ] 新增定时任务
- [ ] 支持悬浮对话框
- [ ] 支持 Web Search
- [ ] 支持 Dots
- [ ] 优化自动上下文压缩
- [ ] 丰富插件体系
- [ ] 工具性能优化
- [ ] 支持 Agent Trace
- [ ] Worktree 功能优化
- [ ] 支持自定义命令和Hooks
- [ ] SSH 远程开发优化
- [ ] 支持子 Agent 和父 Agent 的实时通信
- [ ] 支持子会话和会话间通信

## Later

- [ ] Markdown 可视化编辑器（暂缓实施，保留 [REA 研究结论](docs/cursor-markdown-editor-research.md)）
- [ ] 支持浏览器端

## Out

- Collaboration, calls, channels
- Copilot
- Edit prediction
- Vim, which-key, REPL, journal, onboarding
- Zed accounts, feedback, crash reporting, and the auto-update UI (`auto_update` stays for remote-server downloads)
