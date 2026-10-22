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
  - [x] 支持 Cursor Debug 模式
  - [x] 支持输出内容选中添加到会话
  - [x] 支持模型 token 速率展示，缓存率展示
  - [x] 支持 Ask Question 工具
  - [x] 命令审批工具优化
  - [x] 支持 Composer 预测，Tab 自动接收，https://learn.chatgpt.com/docs/features/composer-predictions
  - [ ] 研究 Cursor 提示词和工具提示词，分析模型在 Cursor 中能输出高质量总结的原因，进行对齐（部分已对齐）


  **新增问题**

  - [x] Ask Question 工具没对齐 cursor
  - [x] 命令审批工具没对齐 cursor
  - [x] Composer 预测的内容和会话内容无关
  - [x] 会话 fork 会导致闪退
  - [x] 需要移除滚动到 Top
  - [x] 会话结束后的操作按钮间距加大
  - [x] 输出结束后 Worked 默认折叠

- [x] 右侧边栏优化
  - [x] 分类项的菜单默认隐藏，鼠标悬浮后展示
  - [x] 右侧边栏增加 tab 状态，Agent Tab 增加 Agent 运行状态，终端面板增加命令执行状态，文件 Tab 支持显示文件图标

- [x] 文件 Panel 优化
  - [x] 没选中文件时隐藏缩进线
  - [x] 项目选择下拉和 分支/worktree 选择下拉更新为 gpui-kit 组件
  - [x] 右键菜单替换为 gpui-kit 组件，增加 Add to chat 选项
  - [x] 支持单文件 Diff 视图
  - [x] 文件 Diff 样式优化

- [x] Thread Panel 优化
  - [x] 去掉 New Thread 按钮和 New Terminal 按钮
  - [x] 去掉下拉菜单，不允许在 Thread Panel 切换项目

## Next

- [ ] 全局优化
- [ ] 设置菜单优化
- [ ] 新增定时任务
- [ ] 支持悬浮对话框
- [ ] 重新设计 App 图标
- [ ] 支持 Web Search
- [ ] 支持 Dots
- [ ] 优化自动上下文压缩
- [ ] 丰富插件体系
- [ ] Markdown 拓展，支持可视化 Markdown 编辑器
- [ ] 工具性能优化
- [ ] 支持 Agent Trace
- [ ] Worktree 功能优化
- [ ] 完善更新图标合集
- [ ] 支持自定义命令和Hooks
- [ ] SSH 远程开发优化

## Later

- [ ] 支持浏览器端

## Out

- Collaboration, calls, channels
- Copilot
- Edit prediction
- Vim, which-key, REPL, journal, onboarding
- Zed accounts, feedback, crash reporting, and the auto-update UI (`auto_update` stays for remote-server downloads)
