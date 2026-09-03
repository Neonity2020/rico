# rico 官方 Landing Page 设计规范 (DESIGN.md)

本文档定义了 `rico` 官方 Landing Page（基于 Astro + Tailwind CSS）的视觉风格、设计语言、组件规范与页面结构。

---

## 1. 品牌定位与设计基调 (Brand Identity & Theme)

- **产品定位**：原生 Rust 极简无框架轻量 Coding Agent，毫秒必争、极致性能、专注工程落地。
- **视觉基调**：**深色极客暗黑风 (Sleek Dark / Linear & Raycast 风格)**
  - 黑曜石深空底色，避免刺眼的纯黑，提供细腻的深灰渐变与环境微光；
  - 继承终端 TUI 经典青绿强调色 (`#6EBEB4`)，贯穿按钮、边框发光与模拟终端的高亮；
  - 玻璃拟态 (Glassmorphism)、微发光边框 (Subtle Glow Border)、现代无衬线西文字体结合等宽终端字体。

---

## 2. 调色板 (Color Palette)

### 基础背景与表面色 (Background & Surfaces)
| 变量名 | HEX / 描述 | 用途 |
|---|---|---|
| `bg-primary` | `#0B0D13` | 页面主背景（黑曜石暗夜底色） |
| `bg-secondary` | `#131722` | 卡片表面底色、次级区块容器 |
| `bg-elevated` | `#1C2234` | 悬浮卡片、输入框、下拉浮层底色 |
| `bg-terminal` | `#0F121C` | 终端模拟器背景底色 |

### 强调与品牌色 (Accent & Brand Colors)
| 变量名 | HEX / 描述 | 用途 |
|---|---|---|
| `accent` | `#6EBEB4` (Teal) | 核心品牌色、行动按钮 (CTA)、重要指示器、TUI 原生主调 |
| `accent-light` | `#8FE2D8` | 悬浮高亮、光晕点缀 |
| `accent-glow` | `rgba(110, 190, 180, 0.15)` | 终端与主视觉卡片外发光 |
| `accent-gradient`| `linear-gradient(135deg, #6EBEB4 0%, #3B82F6 100%)` | 渐变大标题与重点徽章 |

### 文本与排版色 (Typography)
| 变量名 | HEX / 描述 | 用途 |
|---|---|---|
| `text-primary` | `#F3F4F6` (Gray 100) | 一级标题、主要正文 |
| `text-secondary` | `#9CA3AF` (Gray 400) | 次级描述、特性说明 |
| `text-muted` | `#6B7280` (Gray 500) | 辅助标注、版本信息、键盘快捷键 |

### 状态指示色 (Functional & Status)
| 变量名 | HEX / 描述 | 用途 |
|---|---|---|
| `status-success` | `#10B981` (Emerald) | 缓存命中 (Cache Hit)、测试全通 |
| `status-warning` | `#F59E0B` (Amber) | Token 压缩中、工具调用中 |
| `status-info` | `#60A5FA` (Blue) | 会话路径、版本标签 |

---

## 3. 字体与排版层级 (Typography System)

- **主要正文/标题字体**：`Inter`, `-apple-system`, `BlinkMacSystemFont`, `PingFang SC`, `Hiragino Sans GB`, sans-serif
- **代码与终端字体**：`JetBrains Mono`, `Fira Code`, `SF Mono`, monospace
- **排版规格**：
  - **Hero 主标**：`clamp(2.5rem, 5vw, 4.5rem)`，粗体，搭配流光渐变 `bg-clip-text`；
  - **副标题**：`1.25rem ~ 1.5rem`，行高 1.6，优雅灰色；
  - **区块标题**：`2rem ~ 2.5rem`，Semibold；
  - **终端/代码**：`0.875rem ~ 0.95rem`，等宽，行高宽松（1.7）。

---

## 4. 页面结构与核心板块 (Page Structure)

页面采用**全景式产品展示架构**，单页直通流式叙事：

```text
┌──────────────────────────────────────────────────────────┐
│ 1. 顶部导航 (Sticky Glass Navbar)                        │
│    - Logo [rico] + 版本胶囊 + GitHub Star + 快速开始按钮 │
├──────────────────────────────────────────────────────────┤
│ 2. Hero 展区 (The Hook)                                  │
│    - 醒目标题："极致轻量的原生 Rust Coding Agent"        │
│    - 一键安装脚本复制框 (cargo install / brew / bash)    │
│    - 核心亮点胶囊：0 运行时依赖 · 毫秒冷启 · 缓存命中 85%│
├──────────────────────────────────────────────────────────┤
│ 3. 实时交互/动态终端演示 (Interactive Hero Terminal)     │
│    - 仿真实端：真实模拟 rico TUI 界面                   │
│    - 动态展示：自然语言需求 -> 思考过程 -> 自动工具调用 │
│      -> 代码编辑 -> Prompt Cache 命中指标展示 (CH85.2%)  │
├──────────────────────────────────────────────────────────┤
│ 4. 核心特性网格 (Bento Grid Architecture)                │
│    - [原生性能]：纯 Rust 零垃圾回收，毫秒响应            │
│    - [Prompt Cache]：智能上下文复用，成本直降 70%+       │
│    - [四大编程沙箱]：read / write / edit / bash          │
│    - [全双工会话]：JSONL 崩溃容错存储，断点无缝续接      │
├──────────────────────────────────────────────────────────┤
│ 5. 极速上手与 CLI/TUI 对比演示                           │
│    - TUI 全屏交互模式 vs 传统 stdin/stdout CLI REPL      │
│    - 命令菜单指南 (/cache, /session, /clear)             │
├──────────────────────────────────────────────────────────┤
│ 6. 底部行动区与页脚 (CTA & Footer)                       │
│    - "开始用 Rust 驾驭智能编码"                          │
│    - 开源协议、GitHub 链接、文档索引                     │
└──────────────────────────────────────────────────────────┘
```

---

## 5. 标志性视觉组件规范 (Signature Components)

### 5.1 动态模拟终端 (Interactive Simulated TUI)
- **外观**：macOS 风格三色圆点红黄绿控制区，顶部居中标题 `rico — kr/claude-sonnet-4.5 — 约 1,234 词元 · CH85.2% · 就绪`；
- **边框**：1px `border-white/10` + `shadow-[0_0_50px_-12px_rgba(110,190,180,0.2)]` 外光晕；
- **动态特性**：
  - 自动打字机效果模拟用户提问；
  - 模拟思考过程折叠气泡 `<think>`；
  - 仿照 `rico` 实装的 UTF-8 闭合边框代码块；
  - 底部状态栏展示实时词元统计与 `CH` 缓存指示器。

### 5.2 Bento 特性网格卡片 (Bento Grid Cards)
- **材质**：深黑背景 + 10% 白色内描边 + 鼠标悬浮微发光跟踪；
- **排版**：不等宽 3+2 布局，主卡片配互动式小型可视化（如缓存节省对比柱状图、工具流水线示意）。

### 5.3 复制命令行组件 (Command Copy Snippet)
- **样式**：等宽字体黑底卡片，前缀 `$`，右侧一键复制并展示 "已复制" Tooltip 动画。

---

## 6. 技术栈与工程规范

- **框架**：[Astro 5.x](https://astro.build/)（静态生成 SSG，零客户端 JS 水合开销，极致加载速度）
- **样式**：[Tailwind CSS](https://tailwindcss.com/)
- **图标**：Lucide Icons
- **响应式断点**：
  - 移动端 `< 640px`：单列瀑布流，模拟终端自适应紧凑模式；
  - 平板 `640px ~ 1024px`：双列网格；
  - 桌面 `> 1024px`：全景展开，终端与控制区全尺寸展示。
