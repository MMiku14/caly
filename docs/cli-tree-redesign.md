# caly CLI 命令树重设计(v2 提案)

> 状态：设计文档（尚未实施）。实施前的对齐基准是当前 grammar.rs（v1 树）。
> 日期：2026-08-10。作者：本轮审计/优化工作流。

---

## 1. 背景与目标

当前 CLI(v1）源自三轮迭代：顶层五个命名空间 `daemon / tool / show / set /
completions`。功能上覆盖了 daemon 生命周期、读写配置、订阅/节点/组管理与
诊断，但存在**同类语义被拆到多个命名空间**、**资源/动词混排**、**离线/在线
视图重名**三类结构性问题。本文给出 v2 目标树、迁移映射与分期路线。

设计目标（按优先级）：

1. **一个语义只在一个位置**（单一事实源）：任一动作的归属可预测。
2. **读写分离硬边界**：`show`/`diag` 永不写磁盘/不改 daemon 状态；`set`
   一律 `--dry-run` 默认、`--apply` 生效。
3. **离线/在线不混淆**：配置（磁盘）与内核（活快照)是两种数据源，命令路径
   直接区分。
4. **可机器消费**：所有 `show`/`diag` 叶子支持 `--json`,JSON 结构稳定
   （向后兼容地加字段、绝不改字段语义）。
5. **平滑迁移**：旧路径保留 alias，一个版本期后弃用，再一个版本期移除。

---

## 2. v1 现状盘点（grammar.rs 实际树)

```
caly daemon                                  # 前台运行(无子命令)
caly tool help [cmd] | version | doctor [--fix] | tui [--readonly] | dns [domain]
caly show status
      show core nodes|groups|connections|traffic|mode|rules [--match T]|health
      show sub  providers|parse <path> [--userinfo U]|import [path|--clipboard]
      show profile list|show <id>
      show proxy list|show <id>|groups       # ← 离线内联节点
      show config path|files|validate [--file]
caly set  core start|stop|restart|switch <core>|select [<id>|--delay]|mode <m>|
               close-connections|delay [<name>|--all] [--url U] [--samples N]|
               url-test <name> [--url U] [--samples N]
      set  proxy add <uri> [--group G]|edit <id>|remove <id>|import <path>|on|off
      set  tun on|off
      set  sub  refresh|add <url> [--name N]|remove|enable|disable|import [path|--clipboard]
      set  profile add <id> <src>|remove <id>|edit <id>|refresh [id]|
                 export <id> --out <path>|enable <id>|disable <id>
      set  config apply|generate|default|diff [--file]|edit <editor>
      set  daemon stop|reload|restart|status
      set  rule-provider add-http|add-file|add-inline|remove|enable|disable|refresh|list
      set  proxy-group add|remove|enable|disable|list …
      set  provider add-sources <name>|add-nodes <name> <URI>…|remove <name>
caly completions <bash|elvish|fish|powershell|zsh>
全局旗标:--socket --json --core --mihomo-bin --sing-box-bin
写叶子统一带互斥的 --dry-run(默认)/--apply
```

---

## 3. 现状问题清单

| 编号 | 问题 | 例证 |
|------|------|------|
| C1 | **daemon 生命周期被劈成两半** | 运行 = `caly daemon`（顶层叶子）；停止/重载 = `caly set daemon stop` |
| C2 | **诊断与写操作混居** | `set core delay` / `set core url-test` 是只读测量却挂在写命名空间下 |
| C3 | **同名异源的 show** | `show proxy list`（离线内联节点）vs `show core nodes`（在线活节点）——名字像、数据不同 |
| C4 | **同叶多职** | `set proxy on/off` 是系统代理开关，`set proxy add` 是内联节点写入——一个名词两类动作 |
| C5 | **动词不一致** | `close-connections`（动宾）vs `start/stop`（动词）vs `add-*`（动+名）混排 |
| C6 | **自指式命名** | `show profile show <id>`、`provider add-sources`/`add-nodes` 与 `rule-provider add-*` 形态不一 |
| C7 | **dns 子命令能力单薄** | 仅 `tool dns [domain]`；不能指定服务器/超时（本轮模块已支持并行探测+延迟，CLI 未透出） |
| C8 | **顶层叶子/命名空间混排** | `daemon` 是裸叶子，`show/set/tool` 是命名空间——形状不规整 |

---

## 4. v2 设计原则

- **资源域优先**：`caly <域> <动作>`。六个顶层域：`daemon · show · set ·
  diag · tool · completions`。
- **动作词汇表收敛**：CRUD = `add/edit/remove/enable/disable/list`；生命周期 =
  `start/stop/restart/switch/reload`；同步 = `refresh/import/export`；开关 =
  `on/off`；选择 = `select`；诊断只读动词 = `doctor/dns/latency/url-test`。
- **在线 vs 离线**：在线活状态一律在 `show` 直挂（`show nodes`）；离线配置
  一律在 `show config` 之下（`show config nodes`)。
- **测量不入 set**：任何不改状态的探测一律进 `diag`。
- **退出码契约**(v1 已有事实，v2 固化）：

| 码 | 语义 | 使用者 |
|----|------|--------|
| 0 | 成功 | 全部 |
| 1 | 执行失败（daemon 不可达/写失败/校验失败） | 全部 |
| 2 | 用法错误（clap 解析失败、参数互斥） | clap + 显式返回 |
| 3 | 诊断未通过（doctor/dns 探活失败） | `diag` 族 |

---

## 5. v2 目标命令树

```
caly daemon run                              # 前台运行(原裸 caly daemon)
      daemon stop                            # ← 自 set daemon 迁入
      daemon reload
      daemon restart
      daemon status                          # 轻量存活探询(离线可用)
caly show status                             # daemon 汇总(在线)
      show nodes                             # 在线节点(原 show core nodes)
      show groups                            # 在线组(原 show core groups)
      show connections                       # 活跃连接
      show traffic                           # 累计流量
      show mode                              # 当前路由模式
      show rules [--match <target>]          # 规则列表/逐条求值(在线)
      show health                            # daemon+内核健康摘要
      show profiles                          # 离线:profile 列表(原 show profile list)
      show profile <id>                      # 离线:单个 profile(消灭双 show)
      show config path|files|validate [--file]
      show config nodes                      # 离线:内联节点列表(原 show proxy list)
      show config node <id>                  # 离线:内联节点详情(原 show proxy show)
      show config groups                     # 离线:声明的 proxy_groups(原 show proxy groups)
      show sub providers                     # 离线:订阅源
      show sub parse <path> [--userinfo U]   # 离线:解析订阅文件
      show sub import [path|--clipboard]     # 离线:URI 清单预览
caly set  core start|stop|restart|switch <mihomo|sing-box>
      set  mode <rule|global|direct>
      set  select [<node-id>|--delay]        # 选节点(可 --delay 择最快)
      set  node add <uri> [--group G]        # ← 自 set proxy add 迁入,--apply
      set  node edit <id>
      set  node remove <id>
      set  node import <path>                # ← 自 set proxy import 迁入
      set  connections close                 # ← 原 set core close-connections
      set  sysproxy on|off                   # ← 自 set proxy on/off 拆出(系统代理)
      set  tun on|off
      set  sub refresh | add <url> [--name N] | remove <url> |
                enable <url> | disable <url> | import [path|--clipboard]
      set  profile add <id> <source> | remove <id> | edit <id> |
                refresh [<id>] | export <id> --out <path> |
                enable <id> | disable <id>
      set  config apply | generate | default | diff [--file] | edit <editor>
      set  proxy-group add <name> --type <KIND> --members <spec>…
                [--url U] [--interval-seconds N] [--tolerance-ms N]
      set  proxy-group remove|enable|disable <name>
      set  proxy-group list                  # 唯一例外:list 归 set(成员写向域)
      set  rule-provider add-http <name> <url> | add-file <name> <path> |
                add-inline <name> <规则>… | remove|enable|disable <name> |
                refresh [<name>] | list
      set  provider add-sources <name> | add-nodes <name> <URI>… | remove <name>
caly diag doctor [--fix]                     # ← 自 tool 迁入
      diag dns [<domain>] [--server NS]… [--timeout MS]   # ← 自 tool dns 迁入+增强
      diag latency [<node>|--all] [--url U] [--samples N] # ← 自 set core delay
      diag url-test <node> [--url U] [--samples N]        # ← 自 set core url-test
caly tool tui [--readonly]
      tool version                           # ← 自 tool help/version 拆出提升
      tool help [<command>]
caly completions <bash|elvish|fish|powershell|zsh>
```

### 5.1 `diag dns` 增强对接（本轮 DNS 模块已落地）

模块层新增：并行探测（每服务器一线程）、逐服务器延迟测量、IPv6 地址族修正、
ICMP 拒绝与超时区分开。CLI 相应透出：

| 旗标 | 语义 | 默认 |
|------|------|------|
| `--server NS`（可重复） | 指定探测对象（裸 IP/`host:port`/`tls://…`) | `CALY_DNS_NAMESERVERS` → 配置 `dns.nameservers` → 内置公共列表 |
| `--timeout MS` | 单服务器读写超时 | 3000ms |
| `[domain]` | 探测域名 | example.com |
| `--json` | `{"ok","domain","results":[{nameserver,status,latency_ms,detail?}],"truncated"}` | — |

> 服务器列表上限 16（与 domain 组界一致），超出截断并在 JSON 里给
> `truncated: true`；人类模式 stderr 提示。

### 5.2 校验规则（不变量）

- `--apply`/`--dry-run` 互斥，所有 `set` 叶子必带其一（默认 dry-run)。
- `set profile refresh` / `set rule-provider refresh` 例外：总是写（v1 已有
  决策——refresh 的 body 即缓存，无 dry-run 语义）。
- `diag`/`show` 任何叶子都不得触写路径（代码纪律脚本可加 grep 断言）。

---

## 6. v1 → v2 迁移映射

| v1 路径 | v2 路径 | 迁移策略 |
|---------|---------|----------|
| `caly daemon` | `caly daemon run` | alias：裸 `daemon` 隐藏转发，打印弃用警告 |
| `set daemon stop/reload/restart/status` | `daemon stop/…` | alias 一个版本期 |
| `tool doctor` | `diag doctor` | alias |
| `tool dns` | `diag dns` | alias；新增 `--server/--timeout` |
| `set core delay` | `diag latency` | alias |
| `set core url-test` | `diag url-test` | alias |
| `set core close-connections` | `set connections close` | alias |
| `set proxy add/edit/remove/import` | `set node …` | alias |
| `set proxy on/off` | `set sysproxy on/off` | alias |
| `show core nodes/groups/…` | `show nodes/groups/…` | alias |
| `show proxy list/show/groups` | `show config nodes/node/groups` | alias |
| `show profile list` | `show profiles` | alias |
| `show profile show <id>` | `show profile <id>` | alias |
| 其余 | 原样 | — |

实现要点：clap 侧用 `#[command(visible_alias = …)]` 做平滑期，再在
`cli/convert.rs` 增加一次性弃用告警（`eprintln!("[deprecated] …"`)，两个版本
期后删 alias。

---

## 7. 分期路线

| 期 | 内容 | 风险 |
|----|------|------|
| P1 | `diag` 命名空间落地（doctor/dns/latency/url-test 迁入 + alias);`diag dns` 增 `--server/--timeout` | 低：纯 grammar + dispatch 挪动 |
| P2 | `daemon run` + 生命周期迁入 `daemon` 域；`set connections close`;`set sysproxy` | 低：dispatch 重定向 |
| P3 | `set node`/`show config nodes` 收编 `set proxy`/`show proxy`;`show core` 去层 | 中：帮助文本、e2e、测试大批更名 |
| P4 | alias 移除 + grammar 文档/补全更新 | 低：确认无告警后删除 |

每期都必须：`cargo check --workspace --all-targets` · `cargo clippy
--workspace --all-targets`(0 警告）· `cargo test --workspace` · 6 个纪律脚本
全绿，并更新 `completion` 生成快照看板。

---

## 8. 非目标（本版明确不做）

- 不引入新的远程协议子命令（如 `logs` tail、`watch` 的 CLI 暴露）;TUI 继续
  承担实时面。
- 不改 `--socket/--json/--core/--mihomo-bin/--sing-box-bin` 全局旗标语义。
- 不为 DNS 增加 DoH 服务器解析能力（模块仍是"探活"定位，不是完整解析器）。
