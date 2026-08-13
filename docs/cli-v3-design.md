# caly CLI v3 设计落档:Entry 一等公民 · 单资源域 · 交互式 CLI

> 状态:设计文档(**已评审拍板,W1 施工中**)。日期:2026-08-11。
> 输入:v2.2 规格(cli-v2.2-design.md,**被本文件取代**)、G 系列策略组设
> 计、sub 模块三段式改进、"取消 group 域"拍板、用户 v3.1 完善稿(本文件
> 主体,HTML 转义与乱码已清理)。
> 对齐基准:`bins/caly/src/cli/grammar.rs`(v1 树)、
> `caly-domain/src/proxy_group.rs`(Entry 模型)、
> `caly-subscription/src/clash.rs`(ClashImport)。
> **落档裁决**(施工期裁定,覆盖/修正输入稿相应条款,逐条见 §17):
> C-A 退出码施工期仍按 0/1/2/3/130(附录 B 扩展矩阵挂起待拍);
> C-B `node pick` 专动词采纳(D17 双参数寻址的细化);
> C-C entries[] 扁平 JSON 采纳为 W3a 目标(取代 G-§3.3 形状,未 ship 先改);
> C-D 附录 E1 为逻辑契约,物理传输 = 既有 JSON-RPC over unix socket;
> C-E corectl 非二进制,E2 链路更正为 bins→server RPC→application→corectl;
> C-F 树面排版律以 G-§3.2 三区制为准(未入组单列区,不采行内标记);
> C-G sub 语义改进(--every/文件源/sub set/--purge/--force/--async/交互
> 命名)落 **W2**,W1 只做机械路由;W1 可落地叶清单见 §12 W1 行。

---

## 目录

1. 权威关系
2. v3 一句话定位
3. 核心模型:Entry 是唯一的资源单元
4. 完整命令树
5. 输出格式规范
6. JSON 契约(冻结,新增只加不改)
7. 交互式回退规范(TTY 检测)
8. 错误信息:可操作(Actionable)
9. 快捷别名系统
10. 上下文感知(Profile)
11. 与 G 系列策略组的精确衔接
12. 实施路线(W1–W4)
13. 纪律 · 依赖 · 预算
14. 风险
15. 评审拍板记录(2026-08-11)
16. 非目标(明确不做)
17. 落档裁决明细(C-A … C-G)

- 附录 A:术语表 / 附录 B:退出码矩阵 / 附录 C:边界用例与防御 /
  附录 D:快速参考卡 / 附录 E:跨组件接口契约

---

## 1. 权威关系

| 文档 | 地位 | 裁决 |
| ------ | ------ | ------ |
| **本文件(v3)** | **命令面 / 交互 / 输出格式 / 错误文案的权威** | 施工基线 |
| `cli-proxy-group-design.md`(G 系列) | **策略组语义权威**:EntryKind、徽章词汇、类型树排版律、corectl 扩展 | 语义保留;命令拼写由本文件吸收;JSON 形状见 C-C |
| `cli-v2.2-design.md` | 前代命令面文档 | **被本文件取代**,留档备查(拍板史) |
| `cli-tree-redesign.md`(v2 提案) | 历史提案 | 归档;C1–C8 问题清单作动机记档 |

不变量承接:v2 退出码契约(C-A 口径)、G-§6 门禁联动、"测试只增不减
(基线 1049)"全部承接。

## 2. v3 一句话定位

**取消 group 特权,所有条目(Entry)平铺为同级资源。**

命令面 = 单资源域扁平树:`node` 域统一承载 protocol / group / builtin
(类型徽章区分),`sub` / `profile` / `config` 各自成域。交互哲学:能打
字就打字,想不起来就交互;错误信息必须回答"什么错了 / 为什么 / 下一步
怎么办"。

## 3. 核心模型:Entry 是唯一的资源单元

```
一级数组: entries[]
  每个 entry:
    - name  (唯一标识,与 node/group 同闭集)
    - kind  (vmess | hysteria2 | … | selector | urltest | fallback |
             loadbalance | relay | direct | reject)
    - type: "protocol" | "group" | "builtin"
    - 仅 group 有: members[](第二级数组,存 name 引用)
```

即 G 系列 EntryKind 哲学贯通到命令面:CLI 不区分 node 与 group,统一叫
**entry**,由 `caly node` 域全域承载。

## 4. 完整命令树

### 4.1 顶层:生命周期与状态(最高频,≤ 2 词)

```bash
caly                      # = caly daemon(前台运行,Q3 拍板)
caly daemon               # 前台运行(同义)
caly stop
caly reload
caly restart
caly status [--verbose]   # 摘要面;daemon 不可达降级离线摘要,500ms 超时
```

### 4.2 条目域:node(统一承载 protocol / group / builtin)

**范式:`caly node [动作] [name] [flags]`**;省略动作 = `select`(交互式
选择器,TTY 下;非 TTY 报 usage),列表需显式 `node list`(2026-08-13
第七轮调整:省略动作从 `list` 改为 `select`,与 `caly n` 别名一致)。

```bash
# ── 查看 ──────────────────────────────────────────
caly node [list]          # 所有条目平铺(混合 protocol/group/builtin)
  [--offline]             # 离线配置视图
  [--type=<kind|protocol|group|builtin>]   # 过滤
  [--format=table|tree|json|compact|name-only]
  [--sort=latency|name|type]
  [--filter=<expr>]       # 例: --filter="type:vmess latency:<100"
caly node show <name>     # 条目详情(protocol:参数+被哪些组引用;
                          # group:成员列表/类型参数/当前选中)

# ── 选择 ──────────────────────────────────────────
caly node select [name]   # 设为全局代理(protocol 或 group 均可)
  [--delay]               # name 为 urltest/fallback 组时选延迟最低成员
caly node pick <group> [member]   # selector 组内手动挑成员(C-B)
  # 省略 member + TTY = 交互弹出组成员列表
  # 对 urltest/fallback 组:报错"该组自动管理,请用 caly node test"

# ── 探测 ──────────────────────────────────────────
caly node ping [name]     # protocol:单节点;group:并行 ping 成员表
  [--url U] [--samples N] [--timeout MS] [--all]
caly node test [name]     # protocol:单节点 URL test;
                          # urltest/fallback:全成员重测并更新选中;
                          # selector:报错"selector 无测速逻辑,请用 caly node ping"
  [--url U] [--samples N] [--timeout MS]

# ── 管理 ──────────────────────────────────────────
caly node add <uri> [--group <group>] [--dry-run | --apply]
caly node add --type <selector|urltest|fallback|loadbalance|relay> \
              --name <name> --members <spec>... \
              [--url U] [--interval-seconds N] [--tolerance-ms N] \
              [--dry-run | --apply]      # 同叶双形态
caly node remove <name|pattern> [--force] [--dry-run | --apply]
  # 被组引用时默认报错并提示;--force 级联剔除
caly node edit <name>     # 编辑(protocol 参数 / group 成员列表)
caly node enable <name>
caly node disable <name>
caly node import <path|--clipboard> [--dry-run | --apply]
```

### 4.3 订阅域:sub(三段式 add,C-G 落 W2)

```bash
caly sub [list] [--enabled] [--disabled]
  [--format=table|json|name-only] [--sort=last-refresh|name|node-count]
caly sub show <name>      # 源/类型/节点数/组数/规则数/刷新周期/状态
caly sub add <url-or-file>
  --name <name>           # 必填或 TTY 交互生成;唯一,与 entry 同闭集
  [--every <hours>]       # 自动刷新周期;默认 24;0 = 纯静态导入
  [--dry-run | --apply]   # 默认 dry-run:校验可下载/可解析,不写入
caly sub refresh [name]   # 省略 = 当前 profile 全部
  [--force] [--async]
caly sub set <name> --url U | --name N | --every H [--dry-run | --apply]
caly sub enable <name> | disable <name>
caly sub remove <name> [--purge]      # --purge 连缓存删;默认保留缓存
caly sub import <path|--clipboard> [--name <name>]   # ≡ add --every 0
caly sub export <name> --out <path> [--format=yaml|json]
caly sub parse <path> [--json] [--userinfo U]        # G1 衔接
```

URL vs 文件自动识别:`http(s)://` 开头 = 远程;否则本地文件(相对路径基
于 `~/.config/caly/subscriptions/`)。

### 4.4 规则 · 连接 · 系统

```bash
caly rules [list] [--match <target>]
caly connections
caly traffic
caly mode [get|<rule|global|direct>]      # 无参 = get
caly sysproxy [on|off|status]             # 无参 = status
caly tun [on|off|status]                  # 无参 = status
```

### 4.5 诊断(动词即命令,不造 diag 域)

```bash
caly doctor [--fix]
caly dns [domain] [--server NS]... [--timeout MS] [--json]   # server≤16
```

### 4.6 配置与 Profile

```bash
caly config [show]
caly config apply
caly config diff [--file <path>] [--format=diff|json]
caly config validate
caly config edit
caly profile [list]
caly profile use <id>     # 切换当前上下文(D10)
caly profile show <id>
caly profile add <id> <source>
caly profile remove <id>
caly profile refresh [id]
caly profile export <id> --out <path>
```

### 4.7 工具

```bash
caly tool tui [--readonly]
caly tool version
caly tool help [cmd]
caly completions <bash|elvish|fish|powershell|zsh>
```

全局旗标不动:`--socket --json --core --mihomo-bin --sing-box-bin`。

## 5. 输出格式规范

### 5.1 自适应输出

| 环境 | 默认行为 |
| ------ | --------- |
| TTY + 窄屏(< 80 列) | 截断长名,保留关键列 |
| TTY + 宽屏 | 全列展开对齐 |
| 管道/重定向 | 自动 TSV(带 header 行) |
| 显式 `--json` | 钉死 JSON |
| 显式 `--format=table` | 强制表格,即使管道 |

缓释:管道 TSV 默认是行为变更(v1 管道 = 人类表),公告一个版本期,
`--format=table` 钉回。

### 5.2 条目列表人类面(在线)

```bash
$ caly node
TYPE         NAME       GROUP        DELAY  STATUS
[vmess]      hk-01      节点选择      28ms   ●
[hysteria2]  sg-02      节点选择      45ms   ●
[trojan]     jp-03      自动选择      112ms  ○
[direct]     DIRECT     —            —       ●
[urltest]    自动选择   —            28ms    ◆
[selector]   节点选择   —            —       ◆
[tuic]       de-01      —            —       ○
```

- `●` = 当前全局选中路径上的叶子(绿);`◆` = 组条目;`○` = 在线不
  在路径(灰);延迟 >300ms 黄,>1000ms 红。

### 5.3 树格式(--format=tree)

排版律以 **G-§3.2 三区制**为准(C-F):策略组区(声明序,嵌套组只显
示 `→ 嵌套组(见 <name>)` 不递归,环标注 `⟲ cycle` 永不 panic)→
未入组节点区 → 规则区(仅离线面有)。

```bash
$ caly node --format=tree
[selector]   节点选择         → 自动选择
  ├─ [urltest]   自动选择     → 嵌套组(见 自动选择)
  ├─ [vmess]     hk-01
  ├─ [hysteria2] sg-02
  └─ [direct]    DIRECT
[urltest]    自动选择         28ms · url=https://cp.cloudflare.com/ · interval=300s
  ├─ [vmess]     hk-01        28ms
  ├─ [trojan]    jp-02        112ms
  └─ [ss]        us-03        205ms
未入组节点
  └─ [tuic]      de-01        —
```

### 5.4 订阅列表人类面

```bash
$ caly sub
NAME       SOURCE                                   TYPE        NODES  GROUPS  LAST REFRESH  NEXT REFRESH  STATUS
airport    https://example.com/sub.yaml             clash-yaml  24     3       2h ago        10h later     ●
backup     ~/.config/caly/subscriptions/backup.yaml clash-yaml  12     0       —             —             ○
```

`●` = 启用且正常;`○` = 禁用;`⚠` = 上次刷新失败(`sub show` 看详
情);NODES/GROUPS 计数来自离线缓存,不触发实时解析。

### 5.5 diff 视图(terraform plan 风格)

```bash
$ caly config diff
~ 自动选择:
  ~ url: "http://old-url.com" → "http://new-url.com"
  - members: [ hk-01 ]
  + members: [ hk-02, sg-01 ]
+ nodes:
  + [vmess] new-node
Summary: 1 modified, 1 added. Run with --apply to sync.
```

## 6. JSON 契约(冻结,新增只加不改)

### 6.1 订阅解析(`sub parse --json`,W3a 落地;C-C 形状)

```json
{
  "format": "clash-yaml",
  "ok": true,
  "counts": {"entries": 12, "protocols": 9, "groups": 2, "builtins": 1, "rules": 3},
  "entries": [
    {"name": "hk-01", "kind": "vmess", "type": "protocol",
     "delay_ms": 28, "groups_in": ["节点选择", "自动选择"]},
    {"name": "自动选择", "kind": "urltest", "type": "group",
     "url": "https://cp.cloudflare.com/", "interval_seconds": 300, "tolerance_ms": 50,
     "members": [{"name": "hk-01", "kind": "vmess", "delay_ms": 28}],
     "selected": "hk-01"},
    {"name": "节点选择", "kind": "selector", "type": "group",
     "members": [
       {"name": "自动选择", "kind": "urltest", "ref": true},
       {"name": "hk-01", "kind": "vmess"},
       {"name": "DIRECT", "kind": "direct"}],
     "selected": "自动选择"}
  ],
  "rules": [
    {"text": "DOMAIN-SUFFIX,example.com,节点选择",
     "target": "节点选择", "target_kind": "selector"}
  ]
}
```

嵌套组标记 `ref: true`(自描述 name);离线面无 delay/selected 时省略字
段(serde skip_none)。冻结自 W3a 出口起。

### 6.2 在线条目列表(`node list --json`)

在 6.1 entry 形状上向后兼容追加 `status`、在线 `members[].delay_ms` 等;
离线省略字段、在线省略 `groups_in` 之外的推导字段,均合法。

## 7. 交互式回退规范(TTY 检测)

需 name 而未给且 stdout/stdin 均为 TTY 时,自动弹出选择列表:

```bash
$ caly node select
? Select entry ›
  [vmess]    hk-01     28ms
❯ [selector] 节点选择  → 自动选择

$ caly node pick 节点选择
? Select member for "节点选择" ›
❯ [vmess]    hk-01      28ms
  [direct]   DIRECT     —
```

非 TTY(脚本/CI;stdin 或 stdout 任一非 TTY 即不交互,附录 C6)立即报
错并提示可用值,不挂起:

```bash
$ caly node select | cat
Error: node select requires <name> in non-interactive mode.
Available: hk-01, sg-02, jp-03, DIRECT, 节点选择, 自动选择
Run with --interactive=0 to suppress this hint.
```

选型 `dialoguer`(Q2 拍板;引入期 W2,四步记账见 §13-1)。TTY 判定 =
`std::io::IsTerminal` + `TERM != dumb` 双闸。取消:Ctrl-C → 130;Esc →
exit 1 `cancelled, nothing changed`。

## 8. 错误信息:可操作(Actionable)

三问必答:什么错了 / 为什么 / 下一步怎么办。样例即契约:

```bash
# 1. 条目不存在(离线有)
$ caly node select hk-99
Error: entry "hk-99" not found in active configuration.
It exists in offline config (group "节点选择").
Run `caly config apply` to sync, or choose from online entries:
  hk-01 (28ms)  sg-02 (45ms)

# 2. 类型误触
$ caly node pick 自动选择 hk-01
Error: "自动选择" is urltest, not selector.
urltest groups are auto-managed.
Use `caly node test 自动选择` to trigger re-test.

# 3. 删除被引用条目
$ caly node remove hk-01
Error: "hk-01" is member of 2 groups: 节点选择, 自动选择.
Remove from groups first: caly node edit 节点选择
Or use --force to cascade delete.

# 4. 订阅名冲突
$ caly sub add https://a.com/sub.yaml --name airport
Error: subscription "airport" already exists.
Use `caly sub set airport --url https://a.com/sub.yaml` to update,
or `caly sub add <url> --name airport2` to rename.

# 5. daemon 不可达
$ caly node list
Error: daemon not responding on /tmp/caly.sock
Start with `caly daemon` or check `caly doctor`.

# 6. DNS 服务器超限
$ caly dns example.com --server ...(17 个)
Error: at most 16 nameservers per probe (you gave 17).
First 16 will be used. Remove --server flags or edit CALY_DNS_NAMESERVERS.

# 7. 文件不存在(sub add 文件源)
$ caly sub add ./nosuch.yaml --name test
Error: file not found: ./nosuch.yaml
Resolved absolute: /home/user/nosuch.yaml
If you meant a URL, include the scheme: https://...
```

类型误触 = exit 2;daemon 不可达 = exit 1(C-A 施工期口径)。

## 9. 快捷别名系统

### 9.1 内置缩写(硬编码)

| 缩写 | 展开 |
| ------ | ------ |
| `st` | `caly status` |
| `n` | `caly node list` |
| `n s <name>` | `caly node select <name>` |
| `n p <name>` | `caly node ping <name>` |
| `n t <name>` | `caly node test <name>` |
| `t` | `caly node list --format=tree` |
| `s` | `caly sub list` |
| `s r` | `caly sub refresh` |
| `d` | `caly doctor` |
| `c` | `caly config diff` |

> **W1 实装口径(2026-08-11)**:除 `t` 外 9 条全部落地
> (`cli/aliases.rs` SHORTCUTS 表);`t` 依赖 `--format=tree`(尚不
> 存在,落地会报用法错),随 W3a 与树面同期进表。
>
> **W3a 实装(2026-08-11)**:`t` 已进表(终值 10 条),展开为
> `node list --format=tree`;W3a 的树面渲染**离线声明树**(在线 wire
> 无组成员关系,W3b 富化同一命令——T-W3a 施工补裁)。

### 9.2 用户别名

`~/.config/caly/aliases.yaml`;展开后首词须为内置命令,否则 exit 2;展开
深度 ≤ 3 防循环;缩写表与 `ClapCommand` 变体名交集为空(断言进
cli_tests)。

## 10. 上下文感知(Profile)

`caly profile use <id>` 切换当前配置上下文;持久化
`~/.local/state/caly/context.json`;tmp+rename 原子写,损坏静默回退
(stderr 警告,回退 default);daemon 重启后有效。`node`/`sub refresh`
无参默认作用当前 profile(消费面 W2)。

## 11. 与 G 系列策略组的精确衔接

| G 系列设计 | v3 落位 | 期 |
| ----------- | --------- | ---- |
| G1 `entry_tree.rs` + 类型树排版 | `caly sub parse <path>`(tree 默认,`--json` = §6.1) | W3a |
| G1/G4 离线组视图复用排版器 | `caly node list --offline --format=tree` | W3a |
| G2 corectl 富化 + mihomo 透出 | `caly node list` / `caly node show` 在线富化 | W3b |
| G3 组内手动选择 | `caly node pick <group> [member]`(dry-run 默认) | W4 |
| G3 组级重测 | `caly node test <group>`(dry-run 默认) | W4 |
| G-Open-Q1 sing-box Clash-API 覆盖核实 | 缺则诚实 `Unsupported`,dual 不回退 | W3b 前置 |
| G-Open-Q2 delays 默认只读缓存 | 采纳;`--measure` 才真测 | W3b |

## 12. 实施路线(W1–W4)

铁律:W1 = 机械路由期(新树 + 别名 + 警告,输出/语义零变化);W2/W3a/
W3b/W4 语义各自独立成期。N2(#125–#131)与 DoT 维持另期,不在 W 期
夹带。

| 期 | 范围 | 出口判据 | 风险 |
| ---- | ------ | ---------- | ------ |
| **W1**(✅ 2026-08-11 收官) | v3 全树 grammar(**仅既有实现可达之叶**,清单下附)+ dispatch 重定向 + 别名展开层(v1→v3 覆盖 100% + §9.1 缩写);裸 `caly`=`caly daemon`;`caly status` 摘要面;退出码纪律;completion 快照;`--offline` 骨架;`profile use` 最小落地(写 context.json) | 门禁全绿;cli_tests ≥1049 只增不减;旧路径全通 + stderr 弃用警告 | 低 |
| **W2**(✅ 2026-08-11;α + β1 + β2a + β2b 全收口) | dialoguer 交互 + 非 TTY 报错面;自适应 table/TSV;错误三问化;●○◆ 徽章与延迟着色;context 消费面;**sub 语义改进批**(β2a schema/三段式/交互命名/§8-4·8-7;β2b name-or-URL 寻址/`sub set`/`remove --purge`/`refresh [name] --force --async`/every 消费链) | 状态机单测 + 非 TTY e2e;PTY 人工核验记档 | 中 |
| **W3a**(✅ 2026-08-11 收官) | `entry_tree.rs`(G1)+ `sub parse` 树化 + `node list --offline --format=tree` + 用户别名 + `config diff --format=diff` | 三 fixture 人工排版 diff + §6.1 契约快照 | 低-中 |
| **W3b** | corectl 富化 + mihomo 透出 + server/application 透传(G2);前置 sing-box 核实 | `node list --json` 在线快照;dual parity | 中 |
| **W4**(✅ 2026-08-12 收官) | `node pick` / 组级 `node test` 两在线写叶(G3);文档回链;旧 alias 警告强化 | dry-run/apply e2e;类型误触 exit 2;成员校验 exit 1 | 中 |

**W1 可落地叶**(既有实现可达,其余叶随其语义期进 grammar):node
list/show/select/ping/test(单节点)/add(URI)/remove/edit/enable/disable/
import、sub list/show/add(URL+--name,同 v1 形)/refresh/enable/disable/
remove/import/parse、rules/connections/traffic/mode、sysproxy on|off(+新
status)/tun 同、doctor/dns、config show/apply/generate/default/diff/edit/
validate、profile 全族 + use、tool 三叶、completions、daemon/stop/
reload/restart/status、rule-provider/provider 平移。**不进 W1**:
pick、组级 test、sub set、--every、文件源 add、--purge、--force/--async、
交互命名、--filter/--sort、diff 视图、树面(随 W2/W3a 同期进 grammar)。

**W4 收官注记(2026-08-12)**:`node pick <group> [member] [--apply|--dry-run]`
与组级 `node test <group> [--apply]` 两在线写叶全落 + 回归修复:

- 链路:grammar → SetCoreCmd::Pick/UrlTest.apply → 离线树校验(组存在/
  selector 类型/成员归属,大小写不敏感,builtin DIRECT|REJECT 规范化)→
  dry-run 预览(exit 0)→ `--apply` 经 `WireCommand::SelectProxyGroup`
  (wire 11,追加于冻结 1–10 集之后)走受管操作;
- daemon 侧:Command::SelectProxyGroup(Arc<str> 保 Copy)+ CoreActor
  后端双实现 —— Mihomo 按显示名直选,sing-box 成员经共享注册表解析为
  `proxy-<hex>` 内核 tag(内置 direct/block、嵌套组名直传),选择持久化
  (selection::remember,仅节点成员);
- 组级 test:urltest/fallback 组 dry-run 预览成员数,`--apply` 复用
  `GET /proxies/{group}/delay` 单节点探测路径(内核侧副作用=重选);
  selector 组 exit 2("no latency logic, 用 node ping");
- 顺带修复(rules.rs 审查遗留):GEOIP/GEOSITE 规则遇显式同 tag
  rule-provider 时渲染为 `rule_set` 引用(此前注释承诺但未实现,静默
  跳过);`GEOIP,private` 保持原生 `ip_is_private`;
- 门禁:check / clippy 0 警告 / **50 suites / 1,164 passed / 0 failed**
  (新增 cli_tests parse 4 + coreconf provider 1 + mock e2e 1);fmt clean。
  明细见 FIX_PROGRESS.md W4 节。

每期:开工前备份 → check → clippy **0 警告** → test(≥1049 只增不减)
→ 7 纪律脚本 → fmt。

**W1 收官注记(2026-08-11)**:α(别名引擎)/ β(全树+填表+删 v1
树)两批全落。出口实测:门禁全绿(check / clippy 0 警告 / test
**1075 passed · 0 failed · 50 suites**,较基线 +26;bin 单测
272→289)/ 7 纪律脚本 / fmt;bins LOC 24,269 → **25,056 / 27,500**
预算。新增落档裁决 C-O / C-P / T-1 与 C-J 落地补记、别名表终值,见
§17;明细见 FIX_PROGRESS.md W1-β 节。

**W2-α 收官注记(2026-08-11)**:输出/错误/降级/投影/消费五项落地,
零新依赖:`--format=table|tsv` 钉回 + 自适应(缺省 TTY=表/管道=
TSV 带 header,Q6);node list/sub list/profile list/离线 groups 四族
表格化 + ●○ 徽章(◆ 组行属 W3b);R1 回链;R5 status 500ms 降级
(exit 仍 1);C-L′ sysproxy|tun status 离线投影;D10′ context 消费面
(CALY_PROFILE > context.json > 无层,daemon 引导单点接入)。门禁全绿:
**1086 passed / 0 / 50**(+11),bins LOC 25,870/27,500。折中与裁决
见 §17(折中编号 Z1–Z4)与 FIX_PROGRESS.md W2-α 节。

**W3a 收官注记(2026-08-11)**:五项交付全落 + 两轮深度审查修复
(baseline review + 三 agent w3a-deep-*):entry_tree 三区制/§6.1
JSON、sub parse 树化、node list --format=tree(+`t` 缩写)、用户别名
aliases.yaml、config diff --format=diff;sub list §5.4 离线派生列
(NODES/GROUPS/LAST REFRESH/⚠,W2-α 挂起项)。审查修复:BUG-1 悬空
组引用误标环、BUG-2 owned-task 错误分级(业务错误不再 daemon 级
fatal——间歇性静默死亡根因候选)、BUG-3 projection 自愈
(try_recover/recover_projection)、风险-1 锁中毒容错、R-1 ANSI 净化
三区补齐、fatal/信号日志化。优化:ping 快速 failover(死节点
45s→15s)、DNS 兜底(TUN 无 dns 段注入 fake-ip 默认)、表格 48 列
封顶截断。门禁全绿:**50 suites / 1159 passed / 0 failed**(+40),
clippy 0 警告, fmt clean。挂起:W3b/W4、traffic 窗口累积(方案①)、
体验优化小项、#65/#66/#67。明细见 FIX_PROGRESS.md W3a 节。

## 13. 纪律 · 依赖 · 预算

1. **依赖**:W2 引入 `dialoguer`(Q2 已拍):①Cargo.toml 注释"Q2
   2026-08-11";②`crate-budgets.toml [external-policy]` 登记
   `dialoguer = ["caly"]`(dependency 脚本 external-policy 机制,勘察实
   证同 reqwest 判例);③unsafe 姿态无涉(forbid 只管本 workspace);
   ④构建障碍回退自研备选(改本文档记档)。
2. **预算**:bins/caly 实测 23,982/25,000(余量 1,018);W1 出口按实测
   +5% 调 toml 注释留档(预期 27,500–28,000;引 crate-replan §5.7,单体
   拆分另行其期)。
3. **哨兵**:读叶永不写 grep 断言随 W1 重写;写面三轨制(在线即时 /
   配置写 dry-run / 组控制 dry-run);`refresh` 例外总是写。
4. **JSON 契约**:§6.1 自 W3a 出口冻结,字段只增不改。
5. **测试**:基线 1049 只增不减;旧路径经别名层全绿;行为变更断言改
   写持平 + 新增覆盖。

## 14. 风险

| 风险 | 缓解 |
| ------ | ------ |
| R1 别名展开后报错错位 | 原 argv 留档,错误回链 `you typed: … → …` |
| R2 TSV 默认冲击脚本用户 | 公告 + `--format=table` 钉回 + 一版本期 |
| R3 dumb 终端交互挂死 | IsTerminal + TERM 双判降级 |
| R4 预算超限 | W1 中期实测,先改 toml 再施工 |
| R5 `caly status` 卡死 | socket 探询 500ms 超时降级离线摘要 |
| R6 context.json 并发/损坏 | tmp+rename 原子写,损坏静默回退 |
| R7 dialoguer 构建障碍 | 回退自研(D18 记账) |

## 15. 评审拍板记录(2026-08-11)

| # | 问题 | 拍板 |
| --- | ------ | ------ |
| Q1 | 实施基线,W1 开工 | **确认开工**(基线由 v2.2 演进为 v3,本文件) |
| Q2 | 交互选择器实现 | **dialoguer**(批准新外部依赖;自研保留回退) |
| Q3 | 裸 `caly` 行为 | **`caly daemon`(前台运行)**;摘要面 `caly status` 承接 |
| Q4 | group 域 | **取消**;组=node 域同级 Entry;组内选择落 `pick` 专动词(C-B) |
| Q5 | sub add 三段式 | 采纳:`<url-or-file> --name <n> [--every <h>]`;URL/文件自动识别;默认 24h,0=静态 |
| Q6 | 输出默认 | TTY=表格,管道=TSV,`--json` 钉死 |

另:**C-A 退出码扩展矩阵未拍**(输入稿附录 B 与冻结契约冲突),施工期
按附录 B 头部裁决执行;如需采纳,单开语叉期。

## 16. 非目标(明确不做)

- 不做 TUI 实时组视图(`tool tui` 后续单独一期复用 JSON 契约)
- 不做 `caly logs` tail(留 TUI)
- 不做组智能地区聚类/延迟重排(违 G-§2-1 忠实文件序)
- 不做订阅合并(profile 域职责)
- 不做自动重命名去重(冲突即报错,不加 `-1`/`-2`)
- 不做订阅内节点编辑(归 `node edit`,sub 只管整份文件)
- 不做 `sub add --dry-run` 内容预览(预览用 `sub parse`)
- 不改全局旗标 `--socket/--json/--core/--mihomo-bin/--sing-box-bin` 语义
- 不引入 DoH/DoT probe(N1 已裁,DoT 单开一期)

## 17. 落档裁决明细(对输入 v3.1 稿的修正)

- **C-A 退出码**:输入稿附录 B(0–7/130/255,daemon 不可达=5、配置
  错=3)**与已冻结契约(v2-§4:0/1/2/3)语义冲突**且声称"全部承
  接"不实。施工期裁决:**仍按 0/1/2/3/130**(0 成功 / 1 执行失败含
  daemon 不可达与 Esc 取消 / 2 用法错误含类型误触·非 TTY 缺参·别名展
  开失败 / 3 诊断未过 / 130 Ctrl-C)。扩展矩阵挂起,单开语叉期评估
  (牵涉既有测试/e2e/脚本断言)。
- **C-B `pick`**:采纳为组内选择专动词(替代 v2.2 文档 D17 的
  `select <group> <member>` 双参数重载)——动词表意无歧义、单/双参
  不歧义;`select` 保持单参在线即时轨。
- **C-C JSON 形状**:采纳扁平 `entries[]` + `members[]` 二级数组
  (Entry 一等公民贯通机器面),取代 G-§3.3 的 `groups[]+ungrouped[]`
  顶形;嵌套组 `ref: true` 替代 `ref: <index>`;**未 ship 先改不产生
  兼容负担**,冻结自 W3a 出口。
- **C-D 传输**:附录 E1 的 REST 式路径为**逻辑契约**;物理传输 = 既
  有 server JSON-RPC over unix socket(新增方法叶,不新造 HTTP 面)。
- **C-E corectl 形态**:caly-corectl 是库 crate 非二进制;实际链路 =
  bins/caly → server JSON-RPC → application 服务面 → corectl trait
  (G-§5.2 扩展点)。
- **C-F 树面排版**:统一采 G-§3.2 三区制(策略组区/未入组区/规则
  区);输入稿 §5.3 行内 `(ungrouped)` 标记不采。
- **C-G sub 分期**:sub 语义改进落 **W2**(交互命名天然同
  期);`--every` 持久化涉 profile schema 字段,开工时先勘 schema;
  W1 不夹带(铁律一:机械路由期不混语义)。
- **C-H `caly core` 小域保留**(输入稿遗漏):v1 `set core
  start|stop|restart|switch <core>` 是**内核**生命周期,与顶层
  `stop/restart`(**daemon** 级)语义不同、必须分域,否则
  `stop` 撞名。v3 增 `caly core start|stop|restart|switch <mihomo|
  sing-box>` 四叶;v1 `set core *` 弃用转发。
- **C-I `caly config path|files` 保留**(输入稿 4.6 未列):v1
  `show config path|files` 能力不丢,平移为 config 域两叶。
- **C-J `caly status` 合并口径**:v1 有双 status(`show status` =
  daemon 状态 / `set daemon status` = quick boot-time)。v3 顶层
  `status` handler = Show(Status);v1 `set daemon status` 弃用转发
  顶层;`--verbose` 于 W2 赋义;500ms 超时降级离线摘要(R5)亦
  W2。
- **C-K `node show` W1 仅离线**(= v1 `show proxy show` handler);
  在线单条目详情随 W3b(corectl 富化同期)。
- **C-L `sysproxy status` / `tun status` 挪 W2**(输入稿 4.4 无著
  但有语法位):platform 层现成查询面待勘;W1 原则"只进既有实
  现可达之叶",新查询面随 W2 语义期。
- **C-M 接缝 S-W1-1(node 单叶双目标分流)**:`node enable|
  disable|remove` 在 v1 分属 `set proxy`(节点)与 `set
  proxy-group`(组)两族;v3 单域合并后 dispatch 需按名分流——
  读取离线配置判定条目类型(同闭集保证无歧义;缺失报 exit 1 +
  可用候选)。这是 W1 唯一新增逻辑(机械路由接缝,不做语义扩
  张);landing 位置 commands/node_dispatch.rs。
- **C-N W1 拆批**:α = 别名引擎(`cli/aliases.rs`,空表恒等接
  线,零行为变化);β = v3 grammar 全树 + 填表 + 删 v1 树 +
  cli_tests 重构 + 门禁 + 预算调值。两批间树保持编译绿。

### W1-β 施工补裁(2026-08-11,随落地入档)

- **C-J 落地形(补记,提前至 W1)**:原文"`--verbose` 于 W2 赋义"
  改判——`caly status` = Show(Status)(daemon 摘要面)与
  `status --verbose` = Show(Core(Health)) **W1 即双双落地**,后者
  与 v1 `show core health` 输出逐字节等价(复用既有 handler);v1
  `set daemon status` 因 delegated 到同一 Health 快照,弃用映射亦落
  `status --verbose`(注释在 aliases.rs 表内)。500ms 超时降级离线
  摘要(R5)仍归 W2 语义期,不提前。
- **C-O `node list --offline` W1 折中**:离线视图 = 既有唯一离线
  handler(声明态 `proxy_groups:` 投影,即 v1 `set proxy-group list`
  语义,convert 落 `Set(ProxyGroup(List))`);节点+组同投影的统一
  离线条目树待 W3a(entry_tree)。`--enabled` 旗标带
  `alias = "enabled-only"`,承接 v1 `--enabled-only` 拼写不断链。
- **C-P `node add` 同叶双形态落地形**:ArgGroup `target`
  (`value`|`name` 恰一且必填);无 `--type` ⇒ 位置参 = 节点 URI
  (`--group` 仅此形态可用,与 `--type` 冲突);有 `--type` ⇒ 位置参/
  `--name` = 组名——v1 `set proxy-group add <name> --type …` 因此只做
  前缀改写(`set proxy-group add` → `node add`),参数面零变化。
  members 逗号分隔复用 v1 `parse_member_token`,url/interval/
  tolerance 诸随组旗标 `requires = "type"`。
- **T-1 过渡叶 `node groups`**:= v1 `show proxy groups` /
  `show core groups`(在线 CoreCmd::ProxyGroups),系 W4 `pick` 与
  组级 `test` 的组名供给面;W3b 富化进 `node list` 后弃用下线(届时
  转入 DEPRECATED 表)。
- **别名表终值(W1-β 实测入档)**:DEPRECATED **75 条**(v1 全命名
  面 100% 覆盖:show 19 / set core 9 / set proxy+tun 7 / set sub 6 /
  set profile 7 / set config 5 / set daemon 4 / rule-provider 8 /
  proxy-group 5 / provider 3 / tool 2);SHORTCUTS **9 条**(§9.1,
  `t` 随 W3a);BUILTIN_HEADS **22 词**(cfg(test) 防撞断言在档)。
  `ProxyGroupKind` 增 `#[value(alias = "selector" | "urltest")]`
  兼容拼写,渲染仍 `select`/`url-test` 不变。

### W2-α 施工补裁(2026-08-11,随落地入档)

- **C-L′ sysproxy|tun status 落地形**:platform 无 OS 态查询面
  (gnome/kde 仅 enable/capture/restore 写面)实证后裁定**离线投
  影**——`declared`(config.yaml 声明态;缺配置=全禁投影,坏配置
  =信封错 exit 1)+ `recovery`(durable 记录存在性=副作用托管
  中)。live OS 查询是新 platform 能力,另期单开。语法位子命令化
  (on/off/status),v1 on/off 拼写与别名面不动。
- **R5 落地形**:`status` 探询(单次 connect 不重试——示踪面不吸
  收启动竞态)+ handshake + snapshot 跑 500ms 墙钟预算;成功渲
  染与既有面字节同源(拆 `render_status_snapshot`);失败渲离线
  摘要(daemon/socket/profile)+ 标准错误信封,**exit 仍 1**
  (C-A 口径下"降级≠成功");新稳定码 `daemon.unreachable`,
  JSON 信封加 profile/socket 键。其他在线命令不降(§8-5)。
- **D10′ context 消费面落地形**:优先级 CALY_PROFILE 环境变量 >
  context.json > 无 profile 层;接入点 = daemon_config::load_from
  单点(daemon 引导与离线读面共用,兑现"daemon 重启后有效");
  stale/corrupt/非法名三态 stderr 警告 + 静默回退(引导永不因客
  户端状态炸)。**范围记档**:离线 node/proxy_group/inline_proxy
  读面现状直读 base config.yaml 不感知 profile 层,其 profile 化
  挂观察项——在线操作随 daemon 引导选举已生效。
- **Q6 落地口径(TSV 契约)**:TSV = header 行(小写 snake_case)
  - TAB 逐字行,缺失值空字段;表头行大写;`--format=table` 强制
  表(即使管道),缺省 TTY=表/管道=TSV。计数行(v1 `nodes: N`
  等)从列表面退役——脚本读 TSV 行数或 JSON `count`。空集:TTY
  保留可操作 hint,TSV 仅 header。
- **§5.2 落地区间**:TYPE/NAME/DELAY/STATUS 四列 + ●(selected=
  快照 desired.selected_node_id)/○;延迟阈值按规格 >300 黄
  >1000 红(v1 阈值 200/500 已改齐,记档变更);**GROUP 列与 ◆
  组行属 W3b**(wire 投影无组成员关系),选中路径高亮同。订阅
  §5.4 的 NODES/GROUPS/LAST REFRESH/⚠ 属 W3a(离线缓存派生)。
- **折中编号(W2 未落项)**:**Z1** §5.1 窄屏截断——需
  terminal-size 类新依赖(unsafe ioctl 违 forbid),依赖候选记档
  另拍;**Z2** CJK 宽字符按 chars 计宽(差一 cell),unicode-
  width 候选;**Z3** rules/connections/traffic TSV 化随 W3b 富化
  同期(遗产 CoreCmd 渲染链不单夹带);**Z4** `profile list` 在
  fresh install(无 config.yaml)报 read 错而非空表(D-W2-2,□
  宽容口径统一点,后续择机小修)。

### W2-β1 施工补裁(2026-08-11,随落地入档)

- **C-Q 交互落地形**:dialoguer 0.12(D18 四步记账:workspace
  manifest 注释/bins 引用/crate-budgets `[external-policy]
  dialoguer = ["caly"]`/构建实证);唯一接缝
  `client/interact.rs`——C6 闸 = stdin+stdout 双 TTY 且 TERM
  非 dumb/非缺省;§7 取消契约 Esc→exit 1
  `cancelled, nothing changed`、Ctrl-C→130 在 picker 生效。
  **交互仅落 `node select` 无参**(空池先报 exit 2 先于 TTY
  判定;非 TTY 报 `Available:` 名单 exit 2);ping/test 无参
  维持 usage。**`--interactive=0` 旗标不落**(§7 样例残留,
  全文无规格)。

### W2-β2a 施工补裁(2026-08-11,随落地入档)

- **Q5 节奏落地形**:schema 新字段
  `SubscriptionSource.refresh_every_minutes: Option<u64>`
  (skip_none,存量配置逐字节 round-trip)。语义:`None` =
  继承批量节奏(`subscriptions.refresh_interval_minutes`,
  #59);**URL 源不传 `--every` → 物化 Some(1440)**(默认
  24h 具象);`--every 0` → 静态钉死;**file 源恒
  Some(0)**,file + 显式 `--every>0` = usage 错
  (`usage.sub.every_on_file` → exit 2)。`--every` 单位小时,
  clap range 0..=1_000_000 防 ×60 溢出;消费链(daemon 周期
  刷新按 per-source 节奏分流)属 β2b。
- **文件源 intake 形**:三段式 `normalize_source_token` =
  http(s) 直连 → `file://` 显式 → 未知 `…://` 拒绝 → 裸路径
  (~ 手工展开不引依赖;不存在 → §8-7 三行逐字;canonicalize
  → `url::Url::from_file_path` 入库存合法 URI)。fetch 半
  `fetch_pinned` 截 file scheme:metadata 预界 + 读后复检双闸
  (复用 `policy.max_body_bytes`),无 etag 每次全读,io 失败
  归 `FetchError::RequestFailed`(transient 判定自然搭上);
  backends `resolve_addresses` 对 file 短路空集。
- **C-R 错误面接缝(ErrorHint)**:新 `crate::output::ErrorHint`
  trait(默认 None),共享 writer(`run_writer` /
  `run_standard_writer`)的 E 升 bound 并在信封 Err 臂统一
  `with_hint`——各资源 dispatch 点零参数扩散(Round 29/30
  收敛不破);7 个错误 enum 补 impl(仅 SubCmdError 真实现)。
  §8-4 `NameTaken`、§8-7 `SourceFileNotFound` 三行(全揉
  message,hint 面保持单行)、"--every on file" usage 错均按
  契约逐字。
- **D-W2-3(schema validate 放行)**:存量校验拒一切非
  HTTP(S) 订阅 URL,与文件源入模直接冲突——sources[] 循环放
  行 `file://`(path 非空),legacy 单字段
  `subscriptions.url` 维持 HTTP(S)-only(测试钉死),错误文
  案更新为"public HTTP(S) URL or a file:// path"。
- **交互命名形(C-G 补完)**:TTY 且未给 `--name` →
  dialoguer Input 提示一次(Enter=跳过,allow_empty);非 TTY
  静默无名。**dialoguer 0.12 的 Input 无 Esc 取消**(无
  `interact_opt`),文本提示仅 Enter/Ctrl-C 两态——§7 Esc 契
  约保持 picker 专属,文本提示不算违约(样例无此面)。
- **依赖白名单**:`url = ["caly", "caly-backends",
  "caly-profile", "caly-subscription"]`(存量 3 点 + 新增
  bins/caly 生成端,不外溢)。

### W2-β2b 施工补裁(2026-08-11,随落地入档)

- **寻址裁定(name-or-URL)**:§4.3 的 `<name>` 在 `set|refresh|
  remove|enable|disable` 五叶统一落"URL 形态精确命中 URL;否
  则 display name 精确匹配;零命中 NotDeclared(exit 1)、多命
  中 AmbiguousName(usage → exit 2,仅手改配置可达)"。'file://'
  按 URL 形态计。寻址解析在 client 离线完成,`refresh <name>`
  未知名不触 daemon。
- **`sub set` 排面**:第 5 动词不进 `ResourceVerb` 4-verb 表;
  ArgGroup(change, required, multiple) 保证至少一变更旗标;改
  URL 转文件源时重钉 `Some(0)`(file 恒静态);悬等值编辑
  = NoChange 零写盘。
- **`refresh --force --async` 语义锚**:force = 服务端清
  memory validators(304 旁路,全拉);async = 提交收条即退
  (exit 0,不轮询终态);per-source id = `subscription_id_for_url
  (存储 URL)`,CLI/daemon 两侧同源计算。
- **every 消费链形**:scheduled 标记只在 application Command 层
  (#59 定时器进程内直交,不经 wire;wire 仅载
  `#[serde(default)] force`);due 纯函数三臂——Some(0) 永不
  /None 恒(继承批量)/Some(m) 距上次成功 fetch ≥ m 分钟;守护
  重启 last 空 = due 一次;scheduled 无 due = 静默成功。批量
  `refresh_interval_minutes` 仍是定时器一人一档(#59 不变)。
- **D-W2-4 观察项**:per-source/scheduled 的 daemon 级 e2e 缺
  口(纯函数与寻址层已测,真 fetch fixture 另期);`sub show`
  细叶评估挂 W3a。

---

### W3a 施工补裁(2026-08-11,随落地入档)

- **T-W3a 树面投影**:`node list --format=tree`(含 bare `node
  --format=tree` 与 `--offline --format=tree`)在 W3a 一律渲染**离线声明
  树**——在线 wire 无组成员关系(W3b 富化同一命令,届时自动升级);`t`
  缩写同源。树面读配置损坏时 exit 1 + stderr(不静默空表)。
- **FormatFlag::Diff**:clap 叶子级 `--format` 与全局 `--format` 同名会
  downcast panic(实测),故 `config diff --format=diff` 复用全局枚举
  (table|tsv|tree|diff);`--format=json` 语义由既有全局 `--json` 承接,
  不新增枚举值。`node list --format=tree` 同理走全局 Tree。
- **aliases.yaml 落地形**:`<config>/aliases.yaml` 的 `aliases:` 映射,
  key 单词、value 字符串(空白分词)或 token 列表;加载校验(空 key/空值/
  目标首词非内置/撞内置头 → 警告+跳过,撞缩写用户优先);展开后 head 非
  内置 → exit 2 + R1 回链;深度 ≤3 复用 MAX_PASSES。
- **sub list §5.4 列落地**:NODES/GROUPS 离线缓存派生(parse cached
  body,不触发实时解析)、LAST REFRESH=缓存 mtime、NEXT REFRESH=
  last+cadence、⚠=启用无缓存(从未成功);TSV header 随列增长(Q6 契约
  变更已落档)。
- **DNS 兜底**:TUN 入站渲染无 dns 段时,coreconf 注入默认 fake-ip DNS
  (223.5.5.5/8.8.8.8 + 28.0.0.1/8,caly-dns::default_tun_dns 单一事实
  源),mihomo/sing-box 双渲染器;有显式 dns 段不动。
- **表格列宽**:Table 模式 48 字符列宽封顶 + `…`(ANSI 完整拷贝不截半);
  TSV 逐字豁免(Z1 折中的静态收敛,不做终端宽度查询)。
- **fatal/信号日志化**:record_fatal 打 error!(?fault);finish_daemon
  fatal 后退出打日志(即使 exit 0);shutdown_signal 区分 SIGTERM/SIGINT。
- **审查修复**:BUG-2(owned-task 错误分级:HandlerFailed/Admission 业务
  级降级,仅基础设施/投影一致性错误 fatal)、BUG-3(projection
  ReliableChannelEmpty 不 poison + try_recover 自愈接线)、风险-1(锁
  中毒 into_inner 容错)、R-1(entry_tree 三区 strip_controls 统一净化)。
- **挂起记档**:traffic 对 sing-box 帧语义错配(方案①短窗口累积读取,
  排期另行);TUN 模式本机代理端口 detour(loopback 表已豁免,跟随
  sing-box 内核修复);ping --all 进度反馈 / connect_with_retry 快速
  失败 / poll_operation 反馈(体验小项)。

## 附录 A:术语表

| 术语 | 定义 |
| ------ | ------ |
| **Entry** | 统一资源单元:protocol(协议节点)/ group(策略组)/ builtin(DIRECT·REJECT) |
| **Kind** | Entry 具体类型:`vmess`…`selector`/`urltest`/`direct` 等 |
| **Type** | 元分类:`protocol` / `group` / `builtin`,用于过滤展示 |
| **Members** | Group 型 Entry 持有的二级 name 引用数组 |
| **Profile** | 配置上下文:一组 entries/subs/rules,可切换 |
| **Dry-run / Apply** | 默认校验预览不写盘 / 显式确认后生效 |
| **Badge** | 人类面 `[]` 类型徽章:`[vmess]`/`[selector]` 等 |
| **G 系列** | `cli-proxy-group-design.md` 及关联设计 |
| **Context** | 当前 profile 标识,持久化 `~/.local/state/caly/context.json` |

## 附录 B:退出码矩阵

> **施工期口径(C-A)**:下表"现行"列绑定;“v3.1 输入稿扩展”列挂起
> 待拍,未采纳。

| 码 | 现行(绑定) | v3.1 扩展稿(挂起) |
| ---- | ----------- | ------------------- |
| 0 | 成功 | 成功 |
| 1 | 执行失败(daemon 不可达/写失败/校验失败;Esc 取消) | 通用错误 |
| 2 | 用法错误(clap 失败/互斥/类型误触/非 TTY 缺参/展开失败) | 误用·无效命令 |
| 3 | 诊断未通过(doctor/dns 探活失败) | 配置错误 |
| 130 | Ctrl-C(SIGINT) | Ctrl-C |
| 141 | BrokenPipe(main.rs `install_broken_pipe_hook` 实证,管道消费者先走) | (同) |
| — | (未分配) | 4 网络/IO · 5 daemon 不可达 · 6 资源冲突 · 7 取消 · 255 内部 |

## 附录 C:边界用例与防御

- **C1 环检测**:`node show/list --format=tree`/`config apply` 时检测;
  标注 `⟲ cycle`,exit 1,永不 panic;文案给 `A → B → A` 与涉事组
  名(渲染层 schema 已在更上游拒绝,此为防御面)。
- **C2 成员引用不存在**:`add --members` 校验存在性,报错给可用条目
  与 `node add <uri>` 指引。
- **C3 空组**:`selector/urltest/relay` 拒空组;`fallback/loadbalance`
  允许(运行时降级 DIRECT)。
- **C4 重名跨类型**:Entry name 全局唯一(与 type/kind 无关);报错附
  既有条目的 kind/type 并指 `node edit`。
- **C5 上下文丢失**:`profile use <不存在>` 报错列可用 profile;
  `context.json` 损坏静默回退 default + stderr 警告。
- **C6 管道+交互命令**:stdin 或 stdout 任一非 TTY 即不触发交互,走
  报错面(§7)。

## 附录 D:快速参考卡

```bash
# 日常高频
caly            # 启动 daemon(前台)
st              # 状态速查
n               # 条目列表
n s hk-01       # 全局选中
n p             # ping 当前选中
n t 自动选择    # 重测 urltest 组
t               # 树形查看
s r             # 刷新全部订阅
d               # 诊断
c               # 配置 diff

# 订阅管理
caly sub add https://a.com/sub.yaml --name airport --every 12
caly sub set airport --url https://b.com/sub.yaml
caly sub refresh airport --force
caly sub remove airport --purge

# 组操作
caly node pick 节点选择 hk-01     # selector 组内手选
caly node test 自动选择           # urltest 全成员重测
caly node show 自动选择           # 组详情

# 配置安全操作
caly config diff                  # 预览
caly config validate              # 校验
caly config apply                 # 应用
caly doctor --fix                 # 自修复
```

## 附录 E:跨组件接口契约(逻辑;物理面见 C-D/C-E)

### E1 CLI ↔ Daemon(逻辑面;JSON-RPC 方法叶一一对应)

`entries.list` / `entries.show {name}` / `entries.select {name}`(全局选
中)/ `groups.pick {group, member}`(G3)/ `groups.test {group}`(G3)/
`subscriptions.list` / `subscriptions.refresh {name?}` / `config.get` /
`config.apply` / `connections.list` / `traffic.get`。socket 默认
`/tmp/caly.sock`(`--socket` 覆盖);`status` 探询超时 500ms,余 30s;
JSON 序列化。

### E2 CLI ↔ Corectl 富化层(G2/W3b)

链路:bins/caly → server RPC → application 服务面 → corectl trait
(G-§5.2 扩字段 `kind/members/delays` + `select_in_group`)。corectl 为库
crate,**无二进制**;返回数据须符合 §6 契约,字段只增不改,CLI 忽略未
知字段。

### E3 CLI ↔ Subscription Parser(G1/W3a)

`parse_file(path, format_hint) → EntryTree + ParseReport`;`sub parse` 与
`node list --offline` 共用同一排版器 `entry_tree.rs`;`--json` 直接序列化
§6.1 形状,不二次转换。

---

> 文档版本:v3.1(落档清理版) | 日期:2026-08-11
> 下一评审点:W1 出口门禁
