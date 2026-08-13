# CLI 重构设计:策略组一等公民与订阅文件类型树排版(G 系列)

> 状态:设计文档(待评审)。落地前的对齐基准:`bins/caly/src/cli/grammar.rs`
> (v1 树)、`docs/cli-tree-redesign.md`(v2 提案,未实施)、本文件只覆盖其中
> **策略组(proxy group)**一条主线,其余 v2 迁移仍按原提案分期。
> 日期:2026-08-11。

---

## 1. 背景与缺口

用户诉求原文:策略组的 CLI 支持;**selector 与 urltest 是与 vmess 同级的两
种类型**;读取订阅文件后,按照**文件自身的类型树**排版划分。

### 1.1 现状盘点(实证)

| 层 | 现状 | 出处 |
|----|------|------|
| 解析 | `parse_clash_config` 全文导入:节点 + `proxy-groups:` + `rules:`(前向引用、重名、未知策略全部校验) | `caly-subscription/src/clash.rs`(`ClashImport`) |
| 域模型 | `ProxyGroup`(5 类型)+ `ProxyGroupMember`(Node/Group/Direct/Reject 闭和)+ `UrlTestConfig` + 完整错误族 | `caly-domain/src/proxy_group.rs` |
| 渲染 | 双核心组块渲染(mihomo `proxy-groups:` / sing-box selector·urltest outbound) | `caly-coreconf` |
| 刷新管线 | 订阅声明了 proxy-groups 即 verbatim 接管拓扑 | `caly-backends/src/config/mihomo_backend.rs` |
| 内核活面 | `KernelControl::proxy_groups()` → `{name, selected}`;**类型、成员列表、各项延迟均不透出**;无组内选择 API | `caly-corectl/src/contract/mod.rs` |
| CLI 写面 | `set proxy-group add\|remove\|enable\|disable\|list`(离线配置) | Round 20 |
| **CLI 读面** | `show sub parse` = `SubscriptionSummary{format,node_count,names:Vec<String>}`——**组与规则在解析后被丢弃**,节点平铺无名分 | `bins/caly/src/subscription.rs` |
| 在线读面 | `show core groups` 只有 组名+当前选中 | 走上面 kernel 面 |

### 1.2 缺口清单(G1..G5)

| 号 | 缺口 | 用户可观察后果 |
|----|------|----------------|
| G1 | 订阅离线视图丢组丢规则 | `show sub parse` 看不到 selector/urltest,也无法核对规则引用 |
| G2 | 类型不同级 | vmess/ss/… 与 selector/urltest 不共享一棵类型树,无法"按类型树排版" |
| G3 | 在线组面单薄 | 无类型/无成员/无延迟;无法判断 urltest 组在测什么 |
| G4 | 无组内手动选择 | selector 组的存在意义(手挑成员)在 CLI 不可达 |
| G5 | urltest 组无手动测量入口 | `set core url-test <node>` 测单节点;组级"立即重测并重选"缺失 |

---

## 2. 设计目标

按优先级:

1. **类型树同源**:CLI 展示的排版结构 = 订阅文件的声明结构,一遍一义;不
   做任何"产品化"重聚类(不按地区、不按延迟默认折叠)。
2. **类型同级**:`selector`/`urltest`(以及 `fallback`/`loadbalance`/`relay`)
   与 `vmess`/`ss`/`trojan`/… 共享同一个类型枚举与同一列"类型徽章"位。
3. **离线永远可读**:不看活 daemon 也能完整盘点一个订阅文件(组、成员、
   规则引用、未入组节点)。
4. **组可操作**:selector 组内选择、urltest 组手动重测,走 daemon 指令面,
   遵守既有读写纪律(`show` 永不写,`set --dry-run` 默认)。
5. **机器可消费**:所有新读面 `--json` 结构稳定(只允许向后兼容地加字段)。

---

## 3. 核心模型:条目类型树(Entry Tree)

### 3.1 统一类型徽章

引入一个**展示层**统一枚举(不进 domain;它是排版词汇,不是领域概念):

```
EntryKind =
  | Protocol(…vmess | vless | trojan | ss | hysteria2 | tuic | wireguard | http | socks5 | shadowtls)   // 叶子:真实节点
  | Group(selector | urltest | fallback | loadbalance | relay)                                          // 组:引用其他条目
  | Builtin(direct | reject)                                                                            // 内置策略
```

落位徽章(人类模式固定宽度,以 `[]` 框起,按字母序无平仄歧义):

| 文件声明(Clash/sing-box) | CLI 徽章 | 类属 |
|---|---|---|
| `type: select` / selector outbound | `[selector]` | Group |
| `type: url-test` / urltest outbound | `[urltest]` | Group |
| `type: fallback` | `[fallback]` | Group |
| `type: load-balance` / loadbalance | `[loadbalance]` | Group |
| `type: relay` | `[relay]` | Group |
| `type: vmess` … | `[vmess]` 等 | Protocol |
| `DIRECT` / `REJECT` 成员 | `[direct]` `[reject]` | Builtin |

命名取舍:CLI 展示用 **selector/urltest**(用户语言、sing-box 同款拼写),
领域与文件层继续用 `select`/`url-test`(`clash_label`);映射只在排版器
一处,保证"文件怎么写,徽章就怎么读"的心智零换算。

### 3.2 排版树(忠实文件序)

```
订阅文件
├─ 策略组区(按文件声明顺序,组可嵌套引用)
│   [selector] 节点选择          成员 4
│     ├─ [urltest] 自动选择      → 嵌套组(见下)
│     ├─ [vmess] hk-01
│     ├─ [hysteria2] sg-02
│     └─ [direct] DIRECT
│   [urltest] 自动选择           成员 3 · url=… interval=300s tolerance=50ms
│     ├─ [vmess] hk-01
│     ├─ [trojan] jp-02
│     └─ [ss] us-03
├─ 未入组节点(不在任何组成员表里的真实节点,按声明顺序)
│     ├─ [tuic] de-01
│     └─ …
└─ 规则区(保留原文件序;每条规则右侧回链其策略目标)
      DOMAIN-SUFFIX,example.com → [selector] 节点选择
      MATCH                     → [selector] 节点选择
```

排版律:

- **声明序即展示序**:组序、成员序、规则序全部逐文件保留(审计/对稿零换算)。
- **嵌套组只展开一次**:被引用的组在其成员位显示 `→ 嵌套组(见 [N])`,
  不递归复制成员;引用序号 = 组区序号。环(理论上 schema 已拒)显示为
  `⟲ cycle` 防御性标注,永不 panic。
- **未入组节点**单列;一个订阅若**没有组**(URI 行/base64 纯节点文件),
  组区与规则区整体省略,排版退化为"纯协议类型清单"——这是合法形态,
  不报"缺组"警告(文件确实没声明)。
- **未知成员/未知策略**:理论上被 schema 校验拦截;排版器对残留态一律
  `[unknown]` 标注并继续,不中断输出。

### 3.3 JSON 契约(冻结字段,新增只加不改)

```json
{
  "format": "clash-yaml",
  "ok": true,
  "counts": {"nodes": 12, "groups": 2, "rules": 3, "ungrouped": 1, "rejected": 0},
  "groups": [
    {
      "index": 1, "name": "节点选择", "kind": "selector",
      "members": [
        {"kind": "group", "name": "自动选择", "ref": 2},
        {"kind": "vmess", "name": "hk-01"},
        {"kind": "direct"}
      ]
    },
    {
      "index": 2, "name": "自动选择", "kind": "urltest",
      "url": "https://cp.cloudflare.com/", "interval_seconds": 300, "tolerance_ms": 50,
      "members": [{"kind": "vmess", "name": "hk-01"}]
    }
  ],
  "ungrouped": [{"kind": "tuic", "name": "de-01"}],
  "rules": [
    {"text": "DOMAIN-SUFFIX,example.com,节点选择", "policy": "节点选择", "policy_kind": "selector", "ref": 1},
    {"text": "MATCH,节点选择", "policy": "节点选择", "policy_kind": "selector", "ref": 1}
  ],
  "userinfo": {"quota_total": 0, "quota_used": 0, "quota_remaining": 0}
}
```

---

## 4. CLI 命令面变更

全部落在 v2 树的既有落位上,**不新造命名空间**;`--json` 一律支持。

### 4.1 变更总表

| 命令 | v1 现状 | 本设计 | 面 |
|------|---------|--------|----|
| `show sub parse <path>` | 平铺 names | **类型树排版**(组/未入组/规则三区)+ `--json` 契约 | 离线读 |
| `show proxy groups`(v2: `show config groups`) | 组名列表 | 同一排版器输出(离线已声明组,成员引用内联节点 id) | 离线读 |
| `show core groups`(v2: `show groups`) | 组名+selected | 富化:kind/members/selected/各项 delay | 在线读 |
| `set select <node-id>` | 选"PROXY 组" | 语义不动 | 在线写 |
| **新增** `set proxy-group select <group> --member <name>` | — | selector 组内手动选择(dry-run 默认) | 在线写 |
| **新增** `set proxy-group test <group>` | — | urltest/fallback 组立即重测(v2 `diag url-test` 族同义,写面不落此处) | 在线写 |
| `set proxy-group add …` | 已有 | 不变(仅 `--type` 增补 selector/urltest 别名输入,规范输出仍 select/url-test) | 离线写 |

### 4.2 关键语义

- `set proxy-group select`:仅当 live 组类型为 Selector 时合法;对 urltest
  组返回用法错误并提示"该组由内核自动选择,请用 `set proxy-group test`"。
  mihomo 走 `PUT /proxies/{group}`;sing-box 走 Clash-API 兼容端点。
- `set proxy-group test`:对 Selector 组同样拒绝(无测可言)。展示返回
  逐成员 fresh delay 与新当选者。
- 两个新写叶子都与既有 `set` 纪律一致:`--dry-run` 打印意图不执行,
  `--apply` 生效;daemon 不可达 → exit 1。

---

## 5. 分层与数据流

### 5.1 离线(本期主战场)

```
订阅文件 ──decode_document──▶ ClashYaml → parse_clash_config ─▶ ClashImport
                                                              │(nodes+groups+rules)
                                                              ▼
                                        bins 展示排版器 entry_tree.rs(新)
                                          │ human/--json
```

- 排版器只依赖 `caly-subscription` 公开类型 + `caly-domain` 徽章词汇;
  **不读 daemon、不碰 fs 以外资源**,与 `show` 纪律一致。
- `SubscriptionSummary.names` 平铺输出被替换;`is_usable` 语义微调:
  有组即算可用(组引用了节点则节点计数天然 >0,兼容旧语义)。
- 三种格式:ClashYaml 走全树;UriLines/Base64UriLines 组区省略(§3.2)。

### 5.2 在线(corectl 扩展,薄改)

`contract::ProxyGroup` 扩字段(向后兼容地加, serde 不影响,内存结构):

```
pub struct ProxyGroup {
    pub name: String,
    pub selected: Option<String>,
    pub kind: Option<String>,        // "Selector" | "URLTest" | … 原样转述
    pub members: Vec<String>,        // mihomo /proxies 的 all[]
    pub delays: Vec<(String, Option<u16>)>,  // 可选:ua 测速缓存透出
}
```

- mihomo `GET /proxies`奥特已经返回 type/all —— 现在是**解析了却没透出**
  (api.rs 已识别 "Selector" 等);改动 = 把已拿到的字段装进 contract。
- 新增 `fn select_in_group(&mut self, group: &str, member: &str, timeout) ->
  Result<(), KernelFailure>`(mihomo `PUT /proxies/{group}`),默认
  `Err(unsupported)`——sing-box 后补,dual 分发按 active_cell。
- application/service 面把新 op 透为既有 ApplicationServicePort 的一条命令
  (词汇沿用 audit 既有代理命令形态),server JSON-RPC 增一叶;
  TUI 暂不接(薄客户纪律,展示侧后续单独一期)。

### 5.3 依赖纪律

全部改动落在既有边上:composition→{corectl, backends},bins→全家;
**无新 crate、无新外部依赖、无 manifest 边变化**,dependency-discipline
与 budgets 预期零调值(bins/caly 预算 25000 内消化)。

---

## 6. 不变量与门禁联动

1. `show sub parse`/`show config groups`/`show groups` 全族只读(纪律脚本
   对 show 族已 grep 写路径,本期增补断言 token)。
2. JSON 契约进 `cli_tests.rs` 的快照断言(字段名冻结;新增字段=新条目)。
3. 排版器纯函数:输入 ClashImport,输出 `String`/`serde_json::Value`;
   行宽、Unicode 组树字符(├ └)在 JSON 面不出现。
4. 退出码沿用 v2 表;`set proxy-group select` 对类型不匹配的组 = 用法错
   exit 2。
5. 每期末尾 7 脚本 + clippy 0 + 全量测试;**测试只增不减**(基准
   1049,N1 后)。

---

## 7. 分期路线(G 系列)

| 期 | 内容 | 出口判据 | 风险 |
|----|------|----------|------|
| **G1** | bins 排版器 `entry_tree.rs` + `show sub parse` 全树化(human+JSON);URI 行退化形态 | 三类 fixture(clash-airport-full.yaml 等)人工排版 diff;测试 ≥ fixture 全覆盖 | 低(纯展示) |
| **G2** | corectl contract 富化 + mihomo 透出 kind/members;`show core groups` 富化;server/application 透传 | `show groups --json` 契约快照;sing-box 路径诚实 `Unsupported` | 中(动内核面,dual 双核心跑 parity 测试) |
| **G3** | `set proxy-group select` + `set proxy-group test`(含 daemon 命令透传、RPC 一叶) | dry-run/apply 语义 e2e;类型误用 exit 2;成员校验失败 exit 1 | 中(在线写,走 supervised op) |
| **G4** | `show config groups` 复用排版器;`set proxy-group add --type` 别名;cli-tree v2 文档互相回链;e2e 补全 | v1→v2 alias 无破损;文档终审 | 低 |

每期独立 commit,门禁 §6-5;G1 不依赖 G2/G3,可先 ship。

---

## 8. 非目标(明确不做)

- 不做 TUI 组视图(薄客户纪律下单独一期,复用同一 JSON 契据)。
- 不做组的"智能地区聚类/延迟排序"等重排版(违背 §2-1)。
- 不动 relay 组的链式编辑 UI(渲染已支持即可,本期只排版展示)。
- 不引入 DoH/DoT probe(N1 评估表已裁:DoT 单开一期)。
- 不实施 v2 树其余迁移(daemon run / diag 命名空间等仍按原提案排期,
  本设计不提前消费)。

---

## 9. 风险与未决

- **R-G1 徽章命名分裂**:用户拼写 selector/urltest vs 文件拼写
  select/url-test。缓解:徽章显示与用户语言一致,JSON `kind` 用同形
  (selector/urltest),文件原文一律保留;两处词汇表在 §3.1 一表封版。
- **R-G2 mihomo 组内选择的"组不在内核"态**:配置里 enable=false 的组、
  或订阅未声明的组,live API 查不到 → `select` 返回 exit 1 + 明确文案
  ("组 X 不在活配置;先 `set proxy-group enable` 或刷新订阅")。
- **R-G3 URI 行订阅无组可展**:退化形态合法化(`§3.2`);不为其虚构组。
- **R-G4 UB 宽度**:组树字符 ├└─ 在 CJK 终端宽度——测试断言按字节宽
  不对齐视觉宽,文档化"树符仅人类面,JSON 面无"。
- **Open-Q1**:sing-box 的 Clash-API 组端点覆盖到哪个版本?G2 前做一次
  兼容性核实(若缺,`select/test` 对 sing-box 诚实 Unsupported,dual 不回退
  mihomo)。
- **Open-Q2**:`show core groups` 的 delays 透出是否默认触发全组测量
  (昂贵)?倾向:**默认只读缓存值**,`--measure` 旗标才真测(与
  `set proxy-group test` 分工:show 永不久等)。
