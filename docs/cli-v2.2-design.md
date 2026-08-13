# caly CLI v2.2 设计落档:命令面 · 交互回退 · 自适应输出 · 可操作错误

> 状态:**留档备查(已演进为 v3)**。后继权威:`docs/cli-v3-design.md`
> (2026-08-11 落档,W1 施工中;吸收 sub 三段式改进、`pick` 专动词、
> entries[] JSON 形状,并对退出码矩阵等做落档裁决 C-A…C-G)。
> 日期:2026-08-11。
> 输入:用户 v2.2 完整规格(原文照录于附录 A;会话中曾迭代 v2.1,未落档,
> 本文件以 v2.2 为准)。
> 2026-08-11 评审拍板:Q1=开工 W1;Q2=交互选型 **dialoguer**(批准新外部
> 依赖);Q3=裸 `caly` 改判为 **`caly daemon`(前台运行)**;Q4=**取消
> group 域**——组与节点是同级条目(EntryKind 哲学贯通到命令面),落 node
> 域,双参数 `<group> <member>` 表达树级寻址(D17)。附录 A 保留输入原
> 貌,被改判条款以正文为准。
> 落地前的对齐基准:`bins/caly/src/cli/grammar.rs`(v1 树,实证于本文 §3)。

---

## 1. 权威关系(先裁决,后施工)

| 文档 | 地位 | 裁决 |
|------|------|------|
| **本文件(v2.2)** | **命令面 / 交互 / 输出格式 / 错误文案的权威** | 施工基线 |
| `cli-proxy-group-design.md`(G 系列,2026-08-11 交付) | **策略组语义权威**:EntryKind、徽章词汇、三区排版律、JSON 契约(G-§3)、corectl 扩展方案(G-§5.2)、写面纪律 | 语义原样保留;其 §4 命令落位与 §7 分期(G1–G4)被本文件 §4/§6 **吸收取代**——命令拼写换,语义不换 |
| `cli-tree-redesign.md`(v2 提案,2026-08-10) | 历史提案 | **整体被 v2.2 取代归档**;其 C1–C8 问题清单仍有效(作为动机记档),P1–P4 分期作废;`diag` 命名空间裁决见 D2 |

不变量承接:v2 提案 §4 退出码契约、G-§6 门禁联动、"测试只增不减(基线
1049)"全部原样承接,见本文件 §7。

---

## 2. v2.2 一句话定位

从"结构正确"(v2/v2.1)进化到"用着顺手":**上下文感知**(profile 当
前语境)+ **交互式回退**(TTY 下缺参弹选择,非 TTY 报错给可用值)+
**自适应输出**(TTY 表格/管道 TSV/`--json` 钉死)+ **可操作错误**(什
么错了/为什么/下一步)。命令面 = **单资源域扁平树**:node 域统一承载
节点与策略组(同级条目,类型徽章区分),sub/profile/config 各自成域
(§3;group 域取消 = D17,用户 2026-08-11 拍板)。

---

## 3. v1(实证)→ v2.2 迁移总表

v1 树实证:`ClapCommand` = `daemon / tool / show / set / completions`
(grammar.rs:80 起;`arg_required_else_help = true` 于 :41)。

| v1 路径(实证) | v2.2 路径 | 迁移策略 |
|---|---|---|
| 裸 `caly`(=help) | **`caly daemon`(前台运行;Q3 拍板)** | 行为变更,见 D1;`caly status` 承接摘要面 |
| `caly daemon` | `caly daemon`(前台;裸 `caly` 同义) | 原样 |
| `set daemon stop\|reload\|restart\|status` | `caly stop\|reload\|restart\|status`(顶层叶子) | alias 表转发 + 弃用警告 |
| `show status` | `caly status [--verbose]`(`--verbose` = 详细健康) | alias |
| `show core nodes` | `caly node [list]`(在线;条目含组,徽章 `[selector]`/`[urltest]` 与 `[vmess]` 同列,D17) | alias |
| `show proxy list\|show <id>` | `caly node list --offline` / `caly node show --offline <id>` | alias;`--offline` 统一离线视图(D6) |
| `show core groups` | `caly node list`(组=同级条目;在线富化=W3b);树面 = `caly node list --format=tree` | alias |
| `show proxy groups` | `caly node list --offline --format=tree`(entry_tree 排版,W3a) | alias |
| `show sub parse <path>` | `caly sub parse <path>`(G1 实体在此,W3a) | alias |
| `show sub providers` | `caly sub [list]` | alias |
| `show profile list\|show <id>` | `caly profile [list]` / `caly profile show <id>` | alias |
| `show core connections\|traffic\|mode` | `caly connections` / `caly traffic` / `caly mode`(无参=get) | alias |
| `show core rules [--match T]` | `caly rules [--match T]` | alias |
| `set core select [<id>\|--delay]` | `caly node select [<id>\|--delay]`(单参=沿用 v1 全局选择语义) | alias;缺 id 且 TTY → 交互(W2) |
| —(组内选择,G3 新叶) | **`caly node select <group> <member>`**(双参数=树级寻址,D17;dry-run 默认) | 新;W4;交互回退(W2) |
| —(组重测,G3 新叶) | **`caly node test <group>`**(urltest/fallback 立即重测;与单节点 URL test 同叶分流——按条目类型分发,dry-run 默认) | 新;W4 |
| `set core mode <m>` | `caly mode [get\|<rule\|global\|direct>]` | alias |
| `set core close-connections` | `caly connections close`(在线即时轨) | alias(D15) |
| `set core delay [name\|--all]` | `caly node ping [id] [--url U] [--samples N] [--timeout MS]`(+`--all`) | alias |
| `set core url-test <name>` | `caly node test <id> [--url U] [--samples N]`(单节点分流同上) | alias |
| `set proxy add\|edit\|remove\|import` | `caly node add <uri>\|edit\|remove\|import`(dry-run 默认) | alias |
| `set proxy-group add <name> --type K …` | **`caly node add --type <selector\|urltest\|…> --name <n> --members <spec>… [--url U] [--interval-seconds N] [--tolerance-ms N]`**(协议节点=URI 位置参;组=`--type`+`--name`,同叶双形态,D17) | alias |
| `set proxy-group remove\|enable\|disable <name>` | `caly node remove\|enable\|disable <name>`(离线条目一视同仁) | alias |
| `set proxy-group list` | `caly node list --offline` | alias;G 设计"list 归 set 唯一例外"与 group 域**一并消失**(D17) |
| `set proxy on\|off` | `caly sysproxy on\|off\|status` | alias(D14:status=新加只读叶) |
| `set tun on\|off` | `caly tun on\|off\|status` | alias(同上) |
| `set sub refresh\|add\|remove\|enable\|disable\|import` | `caly sub refresh [name]\|add\|remove\|enable\|disable\|import` | alias;`refresh` 无参 = 当前 profile 全部(D10) |
| `set profile …` | `caly profile …`(同动词) + **新** `caly profile use <id>` | alias;`use` = 上下文(D10) |
| `set config apply\|generate\|default\|diff\|edit` | `caly config apply\|generate\|default\|diff\|edit`;`show config validate` → `caly config validate` | alias;`diff` 增 `--format=diff\|json` 类 terraform 视图(W3a) |
| `tool doctor` | `caly doctor [--fix]` | alias(顶层动词,D2) |
| `tool dns [domain]` | `caly dns [domain] [--server NS]… [--timeout MS] [--json]` | alias;上限 16 截断(v2-§5.1 承接到此) |
| `tool tui [--readonly]` / `tool version` / `tool help` | `caly tool tui` / `caly tool version` / `caly tool help` | 原样 |
| `completions <shell>` | `caly completions <shell>` | 原样;每期重生成快照 |
| 全局旗标 `--socket/--json/--core/--mihomo-bin/--sing-box-bin` | 不动(附录 A §8 非目标一致) | — |
| `set rule-provider …` / `set provider …` | `caly rule-provider …` / `caly provider …`(资源域平移,语义零变化) | alias(D16) |

---

## 4. 与 G 系列的精确衔接(承接附录 A §六,按 D17 改判落位)

| G 系列设计 | v2.2 落位 | 期 |
|---|---|---|
| G1 `entry_tree.rs` + `show sub parse` 树化 | `caly sub parse <path>`(`--format=tree` 默认,`--json` = G-§3.3 契约) | **W3a** |
| G2 corectl 富化 + mihomo 透出 | `caly node list` / `caly node show <name>` 在线富化(kind/members/selected/delays;组条目同享) | **W3b** |
| G3 `set proxy-group select` | **`caly node select <group> <member>`**(dry-run 默认;交互回退) | **W4** |
| G3 `set proxy-group test` | **`caly node test <group>`**(dry-run 默认) | **W4** |
| G4 `show config groups` 复用排版器 | `caly node list --offline --format=tree`(与 sub parse 共用 `entry_tree.rs`) | **W3a** |
| G-Open-Q1 sing-box Clash-API 组端点覆盖 | W3b 开工前核实;缺则诚实 `Unsupported`,dual 不回退 | W3b 前置 |
| G-Open-Q2 delays 默认只读缓存、`--measure` 才真测 | 采纳(倾向即裁决) | W3b |

**关键修正**(承接附录 A §六末段,因 D17 更进一步):G 文档 `set
proxy-group list` 的"唯一例外"不仅消失——**group 域整体不建**;组回归
G-§3.1 EntryKind 同级哲学,命令面与排版面共用同一棵类型树。

---

## 5. 冲突清单与裁决(D1–D18)

- **D1 裸 `caly` = `caly daemon`(2026-08-11 用户拍板改判)**:v1
  `arg_required_else_help`(grammar.rs:41)→ 裸命令 = 前台运行 daemon
  (clash 系惯例);帮助走 `-h/--help`/`caly tool help`。**状态摘要面由
  `caly status` 承接**(轻量;`--verbose` 详细;daemon 不可达降级离线
  摘要,socket 探询 500ms 超时,exit 0)。cli_tests 裸命令断言改写(计
  数持平);R-W5 相应改写为 status 探询面。
- **D2 `diag` 域取消**:v2.2"动词即命令,不造 diag 域"取代 v2-§5;测
  量动词落 `node ping`/`node test`/`doctor`/`dns`。v2-§5.1 dns 增强
  (`--server` 可重复、上限 16 截断、`--timeout`、`--json` truncated)
  由 `caly dns` 承接。
- **D3 `daemon run` 不加**:保持 v1 形态;C1 修法换为"生命周期叶子顶层
  化"(`stop/reload/restart/status`)+ Q3 裸命令同义。
- **D4 写面三轨制**(纪律级,CLI 全程适用):
  | 轨 | 成员 | 生效 |
  |---|---|---|
  | 在线即时轨 | `node select <id>`、`mode <m>`、`sysproxy/tun on\|off`、`connections close` | 即时无 dry-run(v1 实证 grammar.rs:282–330) |
  | 配置写轨 | `node add/edit/remove/import/enable/disable`、`sub/profile/config/rule-provider/provider` 写叶 | `--dry-run` 默认、`--apply`(实证 grammar.rs:337 起) |
  | **组控制轨(G3 新设)** | `node select <group> <member>`、`node test <group>` | **dry-run 默认**(附录 A §二与 G-§4.2 双源一致;类型误触=exit 2) |
  例外承接 v2-§5.2:`refresh` 族总是写(body 即缓存)。
- **D5 别名 = 预解析 argv 展开层**(取代 v2-§6 纯 clap `visible_alias`):
  visible_alias 无法表达跨层级路径重定向(`show sub parse` → `sub
  parse`)。实现:clap parse 前对 `argv[1..]` 做**前缀最长匹配**查两张
  表——①内置弃用路径表(v1→v2.2 全覆盖,命中 stderr 打印
  `[deprecated] use "caly node" instead`);②内置缩写表(见 D17 修订)+
  用户 `~/.config/caly/aliases.yaml`。展开深度 ≤3 防循环;展开后首词不
  在内置命令集 → exit 2;原 argv 留档供错误回链(R-W1)。
- **D6 `--offline` 统一离线视图**:`node list --offline`、`sub/profile/
  config` 读面;离/在线由旗标直接区分(v2 的 `show config *` 路径不建)。
- **D7 自适应输出契约**:TTY=表格(窄屏 <80 列截断);管道/重定向=TSV
  (带 header 行;**行为变更**:v1 管道=人类表)→ 缓释 = `--format=
  table` 钉回 + 公告一个版本期;`--json` 全局语义无环境钉死。
- **D8 node 树面契约**:`node list --format=tree` = G-§3.2 三区排版(嵌
  套组 `→ 嵌套组(见 N)` 不递归);`--json` = G-§3.3 契约,在线面按
  G-§5.2 向后兼容加 `selected`/`delays`;离线延迟 `—`。表列 =
  `TYPE NAME GROUP DELAY STATUS`(`●` 活跃路径绿,`○` 在线非路径灰;
  >300ms 黄,>1000ms 红)。
- **D9 交互选择器**:落 bins/caly 内部模块(不进 caly-tui,
  presentation-invariants 无涉);状态机抽纯函数单测。**选型已拍:
  `dialoguer`**(2026-08-11 用户批准新外部依赖;W2 引入,引入时过
  external-policy 白名单登记并注释理由)。TTY 判定 = `std::io::
  IsTerminal` + `TERM!=dumb` 双闸;非 TTY 缺参 = exit 2 + 可用值清单
  (附录 A §1.2 样例即契约);取消 = Ctrl-C→130、Esc→exit 1
  `cancelled, nothing changed`。
- **D10 上下文 `profile use`**:纯 bins 客户端状态,
  `~/.local/state/caly/context.json`;tmp+rename 原子写,损坏静默回退
  (永不 panic);无 context 退化 v1(显式参数或报错"先 `caly profile
  use`");daemon 重启后有效。W1 先落 `use` 写入+`list` 显示当前值,
  消费面(`node`/`sub refresh` 默认当前 profile)在 W2。实现先查
  platform 层路径助手(调研点 T1),有则复用。
- **D11 预算调值预告**:bins/caly 实测 **23,982 / 25,000**(余量
  1,018)。W1–W3a 预估净增 1,800–2,600 行(entry_tree ≈450、交互 ≈300、
  别名层 ≈250、自适应/diff ≈500、新 dispatch+测试 ≈800)。**W1 出口按
  实测 +5% 改 `crate-budgets.toml` 并注释**(脚本强制;注释引
  crate-replan §5.7:"CLI 单体拆分另行其期,本期调值消化")。
- **D12 内置缩写碰撞检查**:缩写表与 `ClapCommand` 变体名交集为空的断
  言进 cli_tests(防未来新增内置命令撞车)。
- **D13 退出码表固化**(v2-§4 + 本版增量):0 成功 / 1 执行失败(含
  daemon 不可达、Esc 取消)/ 2 用法错误(含非 TTY 缺参、组类型误触、
  展开失败)/ 3 诊断未过(doctor/dns)/ **130 Ctrl-C**。
- **D14 `sysproxy/tun status` 新只读叶**:附录 A"不带参数=显示状态"落
  成第三动词 `status`;只读纪律天然满足。
- **D15 `caly connections close`**:在线即时轨,无 dry-run(同 v1 实
  证);v2.2 已无 show 命名空间,只读纪律改写为"读叶 grep 断言 + 三轨
  制",哨兵断言 token 随 W1 更新。
- **D16 rule-provider/provider 平移**:`caly rule-provider …` /
  `caly provider …`,v1 `set …` 入弃用表;语义零变化。
- **D17 取消 group 域(2026-08-11 用户拍板)**:组与节点是**不同类型
  树的同级条目**(G-§3.1 EntryKind 哲学贯通命令面):`caly node` 全域
  承载,`select`/`test` 用**双参数 `<group> <member>` 表达树级**;单参
  数沿用 v1 语义(select=全局选择;test/ping=单条目);`add` 同叶双形
  态(URI=协议节点 / `--type`+`--name`=组);list/show/enable/disable/
  remove 对组与节点一视同仁(组名/节点名同闭集,domain 已防重名)。
  内置缩写表相应修订:`n=node list`、`n s=node select`、`n p=node
  ping`、`n t <g>=node test`;`g/g t` 取消(group 域不建);树面缩写
  `t=node list --format=tree`;余 st/d/c/s r 不变。
- **D18 dialoguer 引入记账**:W2 引入时 ①Cargo.toml 注释"Q2 用户批准
  2026-08-11";②external-policy 白名单登记(dependency 脚本扫 manifest
  文本);③clippy/unsafe 姿态不受影响(unsafe_code=forbid 只管本工作
  区 crate);④若 dialoguer 依赖树引入构建障碍(终端库平台面),回退
  自研备选(原倾向),回退须改本文档并记档。

---

## 6. 实施路线(W1–W4,W3 细分)

铁律映射:W1 = **机械路由期**(新树 + 别名层 + 警告,输出/语义零变化);
W2/W3a/W3b/W4 各自语义独立、互不同期。N2(#125–#131)与 DoT 单期维持
另期,**绝不在 W 期夹带**。

| 期 | 范围 | 出口判据 | 风险 |
|----|------|----------|------|
| **W1** | v2.2 全树 grammar(按 D17 单 node 域)+ dispatch 重定向(复用 commands/* 现实现);别名展开层(弃用表 100% 覆盖 v1 + 修订缩写表);裸 `caly`=`caly daemon`(D1);`caly status` 承接摘要面;退出码表;completion 快照;`--offline` 骨架(输出仍 v1 平铺);`profile use` 最小落地(写 context.json,消费面 W2) | 门禁全绿;cli_tests ≥1049 只增不减;旧路径全通且带 stderr 弃用警告 | R-W1/W4 |
| **W2** | dialoguer 交互选择器(D9/D18)+ 非 TTY 报错面;自适应 table/TSV(D7);错误文案三问化主失败面;node 列表 ●○ 徽章与延迟着色(D8);context 消费面(D10) | 状态机单测 + 非 TTY e2e;PTY 交互人工核验记档 | R-W2/W3 |
| **W3a** | `entry_tree.rs` 落地(G1 主体)+ `sub parse` 树化 + `node list --offline --format=tree` 复用 + 用户别名系统(D5②)+ `config diff --format=diff` 视图 | 三 fixture 人工排版 diff + JSON 契约快照(G-§3.3) | 低-中 |
| **W3b** | corectl 富化 + mihomo 透出 + server/application 透传 + `node list/show` 在线富化(G2);**前置:Open-Q1 sing-box 核实** | `node list --json` 在线契约快照;dual parity | 中(动内核面) |
| **W4** | `node select <group> <member>` / `node test <group>` 两在线写叶(G3);文档回链;旧 alias 警告强化(移除排再下一期) | dry-run/apply e2e;类型误触 exit 2;成员校验失败 exit 1 | 中(在线写) |

每期:开工前 `tar czf caly_backup_pre_w<n>.tar.gz --exclude='target'
caly_project/caly`;出口门禁 = check → clippy **0 警告** → test(≥1049
只增不减)→ 7 纪律脚本 → fmt。

---

## 7. 纪律 · 依赖 · 预算影响

1. **依赖**:零新**内部**边;W2 引入 dialoguer = 新外部依赖,**已拍
   (Q2)**,按 D18 四步记账。
2. **预算**:D11;bins/caly 23,982/25,000,W1 出口按实测调 toml(预期
   27,500–28,000,注释留档)。crate-replan §5.7 拆分不提前消费。
3. **哨兵更新**:show 族只读 grep 断言改 v2.2 读叶断言(D15);
   presentation-invariants 无涉(TUI 不接);cli grammar 测试随 W1 重构。
4. **JSON 契约冻结**:G-§3.3 字段只增不改;在线扩展字段向后兼容。
5. **测试只增不减**:基线 1049;旧路径测试经别名层保持全绿;行为变更
   类断言(裸命令、TSV 默认)改写持平 + 新增覆盖。

---

## 8. 风险

- **R-W1 别名展开后报错错位**:clap 报的是展开后新路径 → 原始 argv 留
  档,错误文案回链(`you typed: show sub parse … → caly sub parse …`)。
- **R-W2 TSV 默认冲击既有脚本用户**:缓释 = 公告 + `--format=table` 钉
  回 + 一个版本期(D7)。
- **R-W3 dumb 终端交互挂死**:IsTerminal + TERM 双判降级非 TTY 报错
  (D9)。
- **R-W4 预算超限红门禁**:W1 中期实测一次,先改 toml 再施工(D11)。
- **R-W5 `caly status` 卡死**(Q3 改判后从裸命令移至 status):daemon
  socket 探询 500ms 超时降级离线摘要(D1)。
- **R-W6 context.json 并发/损坏**:tmp+rename 原子写;损坏静默回退
  (D10)。

---

## 9. 评审拍板记录(2026-08-11,全部已拍)

| # | 问题 | 拍板 |
|---|------|------|
| Q1 | v2.2 为实施基线,W1 开工? | **确认开工** |
| Q2 | 交互选择器实现 | **dialoguer**(批准新外部依赖;D18 记账,自研保留为回退备选) |
| Q3 | 裸 `caly` 行为变更 | **改判:裸 `caly` = `caly daemon`(前台运行)**;摘要面由 `caly status` 承接(D1) |
| Q4 | `node select --group` 与 `group select` 重叠 | **改判:取消 group 域**;组=node 域同级条目,双参数 `<group> <member>` 树级寻址(D17) |

---

## 附录 A:v2.2 完整规格(用户输入照录,清理转义)

(以下为权威输入原文;"上一节"指 v2.1,未落档。**裸命令=状态摘要、
group 域两条已被 Q3/Q4 拍板改判,见 D1/D17,正文为准。**)

### A-一、核心新增:上下文感知 + 交互式回退

现代 CLI 的标杆不是 git,而是 docker 和 kubectl:能打字就打字,想不起
来就交互。

**A-1.1 当前上下文(Context)**:引入 profile 作为当前工作上下文,减少
重复参数:`caly profile use airport-1` 切换当前配置上下文;`caly node`
默认显示当前 profile 的在线节点;`caly sub refresh` 不带名 = refresh 当前
profile 全部订阅;`caly group test 自动选择` 在当前 profile 的组里测。
上下文持久化到 `~/.local/state/caly/context.json`,daemon 重启后仍有效。

**A-1.2 交互式回退(TTY 检测)**:当命令需要 ID 但用户没给且 stdout 是
终端时,自动弹出选择列表;非 TTY(脚本/CI)自动报错并提示可用值,不
挂起(`--interactive=0` 抑制提示)。

### A-二、命令树 v2.2(完整版)

顶层状态与生命周期(≤2 词,最高频):`caly`(状态摘要——**Q3 改判为
`caly daemon` 同义,摘要面走 `caly status`**)、`caly daemon`(前台运
行,保持 v1,不加 run)、`stop`、`reload`、`restart`、`status`。

资源域统一范式 `caly <资源> [动作] [id] [--flags]`;省略动作 = list,省
略 id = 交互式选择(TTY 下):

- **node**:`list [--offline] [--format=…] [--sort=latency|name|type|group]
  [--filter=<expr>]`;`show <id>`;`select [id] [--delay] [--group G]`;
  `ping [id] [--url U] [--samples N] [--timeout MS]`;`test [id] [--url U]
  [--samples N]`;`add <uri> [--group G] [--dry-run|--apply]`;
  `remove <id|pattern>`;`import <path|--clipboard>`。
- **group**(**Q4 改判:域取消,并入 node 域**):`list [--offline]
  [--format=tree|table|json]`(tree = G-§3.2 类型树);`show <name>
  [--json]`;`select <group> [member] [--dry-run|--apply]`(G3);
  `test <group> [--dry-run|--apply]`(G3);`add <name> --type KIND
  --members spec… [--url U] [--interval-seconds N] [--tolerance-ms N]`;
  `remove <name>`。
- **sub**:`list`;`show <name>`;`add <url> [--name N]`;`remove <name>`;
  `refresh [name]`(不带名 = 当前 profile 全部);`parse <path> [--json]
  [--userinfo U]`(G1)。
- **规则与连接**:`rules [--match <target>]`;`connections`;`traffic`;
  `mode [get|<rule|global|direct>]`。
- **系统**:`sysproxy [on|off|status]`;`tun [on|off|status]`(不带参数 =
  显示状态)。
- **诊断(动词即命令,不造 diag 域)**:`doctor [--fix]`;`dns [domain]
  [--server NS]… [--timeout MS] [--json]`(server 可重复,上限 16,与
  domain 组界一致)。
- **配置**:`config [show]`;`config apply`;`config diff [--file]
  [--format=diff|json]`(diff = 类 terraform plan 的 +/- 视图);
  `config validate`;`config edit`。
- **profile**:`list`;`use <id>`(切换当前上下文);`show <id>`;
  `add <id> <source>`;`remove <id>`;`refresh [id]`;`export <id>
  --out <path>`。
- **工具**:`tool tui [--readonly]`;`tool version`;`tool help [cmd]`;
  `completions <shell>`。

### A-三、输出格式规范

自适应输出:TTY 窄屏(<80 列)自动截断长名保留关键列;TTY 宽屏全列展
开对齐;管道/重定向自动 TSV;显式 `--json` 钉死 JSON;显式
`--format=table` 强制表格即使管道。

node 列表人类面(在线):列 `TYPE NAME GROUP DELAY STATUS`;徽章
`[vmess]` 等;`●`= 当前活跃路径上(绿),`○`= 在线但不在当前路径
(灰);延迟 >300ms 黄、>1000ms 红。

group 列表 tree 面:与 G-§3.2 完全一致;嵌套组显示 `→ 嵌套组(见 N)`
不递归;延迟在线实时、离线 `—`。(D17 后由 `node list --format=tree`
承接。)

diff 面(terraform plan 风格):`~` 改、`-` 删、`+` 增,末尾
`Summary: N modified, M added. Run with --apply to sync.`

### A-四、错误信息:可操作(Actionable)

必须回答三问:什么错了/为什么/下一步怎么办。样例即契约:①节点不存在
但离线配置里有 → 提示 `caly config apply` 并给出在线可选项;②组类型不
匹配(urltest 不可 select)→ 指向 `node test <group>`/`node show
<group>`;③daemon 不可达 → 指向 `caly daemon`/`caly doctor`;④
`--server` 超 16 → 截断并说明首 16 生效。

### A-五、快捷别名系统

内置缩写(硬编码;D17 修订后):`st=status`、`n=node list`、
`n s <id>=node select`、`n p <id>=node ping`、`n t <g>=node test`、
`t=node list --format=tree`、`s r=sub refresh`、`d=doctor`、
`c=config diff`(原 `g=group list`/`g t` 取消)。用户别名落
`~/.config/caly/aliases.yaml`;展开后首词是内置命令则执行,否则报错。

### A-六、与 G 系列策略组设计的精确衔接

(即本文 §4 表;原文含"关键修正":`set proxy-group list` 例外在资源域
下消失——D17 后进一步,group 域整体不建。)

### A-七、实施路线(一次性替换,不分期 → 本文 §6 细分为 W1–W4)

原表:W1 新命令树 + 旧路径弃用警告;W2 交互式选择 + 自适应输出 + 错误
增强;W3 别名系统 + diff 视图 + G 系列 entry_tree 接入;W4 文档 + 补全 +
旧 alias 警告强化。不变量:旧命令 100% 兼容一个版本期(警告
`[deprecated] use "caly node" instead`);读面永不写;组写面 dry-run 默
认显式 `--apply`;JSON 契约冻结(字段只增不改)。

### A-八、非目标

不做 TUI 实时组视图(`tool tui` 后续单独一期复用 JSON 契约);不做
`caly logs` tail(留 TUI);不做组智能地区聚类(违 G-§2-1 忠实文件序);
不改全局旗标 `--socket/--json/--core` 语义。
