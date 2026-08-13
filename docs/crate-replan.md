# caly crate 结构重规划(v4,实证定稿版)

> 状态:设计文档(尚未实施)。日期:2026-08 10。姊妹文档:`docs/cli-tree-redesign.md`。
>
> **v4 修订记录**(对应第三轮架构复审,全部先实证后落笔):
> 1. **coreconf 依赖证成**(批评一):渲染文件的真实非 domain 依赖=渲染词辈类型
>    + intake↔render 融合便利函数,经三向分流(§5.1)后 `coreconf → {domain, dns}`
>    成立且不再依赖"迁移前的接口重写"。实测:`pipeline.rs` 对 `MihomoProxySet`
>    的引用仅 doc 注释提及,无结构消费。
> 2. **kernel 拆线实证**(批评二):全量文件逐个定性(§5.2);预判的
>    `corectl → coreconf` 边**不存在**(validation 只吃 SpawnSpec+路径,
>    check 用临时配置的渲染在调用方 backends);两侧 mod.rs facade 与
>    mihomo/config.rs 的 publish 半份是仅有的混居点,处置见表。
> 3. **P7 实证**(批评三选 A):application 非 domain/ports 引用面统计
>    (§5.11)——除 composition/ 子树外仅 3 handler 8 处共享 cell,P7 为纯移动。
> 4. **layer 语义澄清**(批评四):层号=依赖偏序,不含运行期中介语义;
>    server → composition 提案**否决留档**(§6,附复活条件)。
> 5. 操作性修订:P1 拆 P1a/P1b;P3 拆 P3a(字节零 diff 迁入)/P3b(typed model);
>    预算改分期收紧(§8);profile 内部禁裸 `Profile` 类型名(§6 注)。
>
> **v4.1 定稿微调**(用户裁决):`caly-profile` 定名 **`caly-profile`**(与既有
> profile_fetch/profile_store 词汇同源);层号顺移:ports/platform 1、template 2
> (v4 将 ports/platform 与 domain 同记 0,会使 ports→domain 沦为同层边,与
> "零 peer 边起步"矛盾——实审赛事,顺移后全图仅剩一条过渡性 peer 边:
> application→backends,P7 灭亡)。
>
> v3 遗产全部保留:双轴模型、铁律二分、B1–B6 编号、M1–M12、词汇表、
> metadata 驱动纪律、预算闸门。

---

## 1. 背景与目标

workspace:**10 crates + 2 bins**。判据不变:

> 每段代码的归属由双轴坐标唯一决定,不允许"两个答案都对";每条边的存在
> 必须有实证(谁 import 谁),不允许"按感觉画图"。

目标:① 一个关注点一个家且名字说真话;② 依赖 manifest 声明、脚本静态审判;
③ 与 size-first 同向;④ 每期可发布、门禁全绿;⑤ 机械改名与语义搬迁永不
同期,结构变更与 bug 修复永不同 commit;⑥(v4 新增)**文档中每条依赖边
都有当轮 grep 实证编号**。

---

## 2. 架构词汇表

| 词 | 定义 | 落点 |
|---|---|---|
| Domain | 纯值对象;禁 std::net/fs/process | caly-domain |
| Port | actor 端口 trait + 跨 actor 协调 cell(P7 起) | caly-ports |
| Backend | Port 的实现 | caly-backends |
| Core | 代理内核(mihomo/sing-box/xray) | CoreKind 一族 |
| **Coreconf** | 核心的**配置**:typed 模型 + 渲染 + golden;纯,零 I/O | caly-coreconf |
| **Corectl** | 核心的**运行**:spawn/control/validate/telemetry | caly-corectl(原 kernel) |
| **Profile** | caly 自身设置:schema/loader/cache/profile 存储(与 profile_fetch/profile_store 同源) | caly-profile(原 config) |
| Platform | fs/process/entropy/tun/uds 唯一触点;raw socket 允许在 capability crate(§5.12) | caly-platform |
| Composition | 组装根(build 期接线,职责终于 listen 之前) | caly-composition |

语言规则:"kernel" 退役;用户可见的"配置文件/config.d"说法不变——
改名只动 crate 语言。profile crate 内**禁裸 `Profile` 类型名**(现有
DnsSettings/MihomoConfigSettings/AppConfig 均已带前缀,保持此约定即无
生态碰撞)。

---

## 3. 现状地图(2026-08-10 实测)

| crate | src 行数 | 内部依赖 | 现状诊断 |
|---|---:|---|---|
| caly-domain | 5 528 | — | 含 DNS 三文件(错位) |
| caly-ports | 309 | domain | 成环实证留档,不并 |
| caly-platform | 4 954 | domain | 含 dns_probe(错位) |
| caly-config | 13 937 | domain, platform | 超载:设置本职(schema/loader/cache/profile 存取)+订阅+模板+核心渲染+net |
| caly-kernel | 3 867 | domain, platform | 混装:管控+渲染 |
| caly-backends | 7 179 | 下层全家 | adapters 集合 |
| caly-application | 9 280 | 全部下层 | composition 内嵌(§5.11 实证) |
| caly-protocol | 2 912 | domain | 不动 |
| caly-server | 2 237 | domain, application, protocol | 不动(§6 决议) |
| caly-tui | 4 094 | domain, protocol | 不动 |
| bins/caly | 23 961 | 全家 | 延后(CLI v2 将重画内部形状) |
| bins/caly-template-worker | 72 | domain, config | 陪绑 reqwest 依赖图 |

三条既有静态禁令:reqwest 仅 config(#75);`cfg(target_os` 仅 platform;
std::net 不入 domain。

---

## 4. 双轴模型与依赖制度

- **role 轴**:domain(0)→ ports(1)/platform(1)→ capability(1)→
  template/coreconf/corectl/profile(2)→ backends(3)/application(3)→
  composition(4)/protocol(4)→ server(5)/tui(5)→ bins(6)。
- **capability 轴**:dns(1)/ subscription(1)/ template(2;消费 platform 故抬一层)。
- **层号语义(v4 澄清)**:layer 是**编译期依赖偏序**,不是运行期调用栈。
  "跳过中间层编号"合法(如 server(5)→application(3)),不存在"绕过"问题。

每 crate manifest 声明:

```toml
[package.metadata.caly]
axis  = "capability"   # 或 "role"
layer = 1
# 同层边才需 allow-peer 声明+理由;跨层只允许 高→低。
```

脚本不变量:**禁环**(cargo metadata 推导);**层序合法**;**reqwest 仅
subscription**;`cfg(target_os` 仅 platform;std::net 不入 domain;
**预算闸门**(§8)。

---

## 5. 诊断与实证(M1–M12;B1–B6;E1–E3)

### 5.1 E1:coreconf 边界实证(第三轮批评一)

渲染迁入文件的非 domain 依赖面(2026-08-10 grep):

| 文件 | 非 domain 依赖 | 性质 |
|---|---|---|
| subscription/sing_box/node.rs | `super::SingBoxOutboundError` | 渲染词辈 |
| subscription/sing_box/document.rs | `SingBoxOutboundError`(词辈)+ `node_to_json/dialable_nodes`(node.rs 自身)+ `decode_document`(**intake**) | 混合 |
| subscription/sing_box/groups.rs | 无(domain::ProxyGroup 等) | 纯 |
| subscription/mihomo/render.rs | `MihomoProxyEntry/Error/Tag`(词辈) | 渲染词辈 |
| config/rule_render.rs(现居 config) | 无(domain+serde_json) | 纯 |

词辈类型出自持方 doc 注释即 "Bounded **rendered** Mihomo proxy block"
(mihomo/mod.rs)——是渲染产物封装,非 intake 事实。`pipeline.rs` 对
`MihomoProxySet` 仅 doc 注释提及,无结构消费(实测)。

**三向分流裁决**:
- 渲染词辈类型(MihomoProxyEntry/Set/Tag/Yaml/GroupName/Error、
  SingBoxOutboundError)**随渲染器迁入 coreconf**;
- 融合便利函数(`uri_body_to_sing_box_json[_with]`、
  `uri_body_to_mihomo_proxy_set`——intake 解码+渲染一步)**迁往唯一消费者
  backends**(`backends/subscription/{http,cached}.rs` 旁),backends 本就
  同时 deps subscription 与 coreconf;
- `decode_document` 等 intake 原件留在 subscription。

**结论:`coreconf → {domain, dns}` 无需 subscription 边、无需把节点类型
上移 domain。** 这就是"选一个,写下来"的选定项。

### 5.2 E2:kernel 拆线实证(第三轮批评二)

逐文件定性(16 个非测试文件,依赖面实测):

| 文件 | 定性 | 归宿 |
|---|---|---|
| mihomo/config.rs | render+validate **纯**;`publish` 走 platform::fs atomic_write | render/validate → coreconf;publish → **backends/config/mihomo_backend.rs**(与 sing_box 一致,该处已自持原子写) |
| mihomo/mod.rs | facade + MihomoSpawnSpecFactory(platform) | facade 消解;spawn → corectl;re-export 面由两 crate 根分别承接 |
| mihomo/api.rs | 纯 serde 模型(io=0) | corectl(control-plane 模型的消费方是 http control) |
| mihomo/http.rs | 有界同步控制客户端(TcpStream×7) | corectl |
| mihomo/runtime.rs | 进程 runtime | corectl |
| sing_box/mod.rs | facade + SpawnSpecFactory(platform)+ **SingBoxConfigRenderer/BaseTuning/SniffOptions(渲染)** | 渲染半份 → coreconf;spawn 半份 → corectl |
| sing_box/dns_render.rs | 纯渲染 | coreconf(R2;P4 换 typed 输入) |
| sing_box/registry.rs | 渲染产物索引 | coreconf(M9 合一) |
| sing_box/http.rs, runtime.rs | 控制/进程 | corectl |
| ports/mod.rs | SpawnSpecFactory/KernelControl/KernelFailure(platform::process) | corectl::contract |
| validation/{mod,linux}.rs | 进程化校验,只吃 SpawnSpec+路径,deps 实测 {domain, platform} | corectl |
| common/mod.rs | descriptor+就绪轮询(platform) | corectl |
| telemetry/mod.rs | 纯限值(运行期邻近策略) | corectl |

**预判裁决:corectl → coreconf 边不存在**——"校验=spawn+临时配置"的
临时配置渲染发生在**调用方 backends**(config/sing_box.rs 先 render 后
validate),validation 本身只认 SpawnSpec+路径。§7 无 peer 边,无偷渡。

### 5.3–5.10(承 v3,M 条目)

- M1 profile(原 config)超载(拆/ M3 渲染归属(coreconf)/ M4 ports 留 /
  M5 contract 更名(P1a 同期)/ M6 net 随订阅(P6)/ M8 node_selection 留 /
  M10 backend 双关(词汇表立法)。
- **M2 → B1–B6**(归宿随 v3:模型/probe 四件入 caly-dns,渲染语义两件入
  coreconf)+ F1(probe 深度)。

### 5.7 M7(修订表述):bins/caly 延后

理由修正为:crate=编译器强制边界,单消费者不是不拆的理由;**但 CLI v2
将重画 bins/caly 的内部形状(grammar/dispatch/诊断命名空间),现在抽边界
是为一星期后即作废的形状付强制成本**。CLI v2 落地期必须重估(bins 内
lib 化或 caly-cli)。本期微操作仅:commands/dns.rs shim 缩回 tool.rs。

### 5.11 E3:P7 投入产出实证(第三轮批评三 → 选项 A 成立)

application 非 domain/ports 引用面(grep 实测,逐文件计数):

```
composition/* 子树        ≈ 114 处(backends 具体类型 / platform / kernel / config)
actors/platform_handler.rs      4 处(SharedDesiredState×2 + 测试 2)
actors/core_handler.rs          2 处(SharedDesiredState)
actors/config_command_handler.rs 2 处(SharedActiveCore)
其余全部(actors/service/operations/supervision/runtime/projection/
command_bus/events/routing/lib)  0 处
```

结论:handlers 一律经 ports trait 分派;具体适配器引用全部封闭在
composition/*。**P7 = 纯移动**(composition/* 成 crate;两个协调 cell
上移 ports;8 处引用改指),不是接口重写。

### 5.12 M12:platform 不变量修订

fs/process/entropy/tun/uds/用户态设备归 platform 唯一;**raw socket
(std::net UDP/TCP)允许出现在 capability crate**(dns probe、订阅 fetch)。
caly-dns 的 txid 由调用方注入——理由是**确定性可测**,不是纯度。
deps 保持 {domain}。

---

## 6. 命名定稿与否决档案(15 crates + 2 bins)

执行:[见 §7 坐标表]。新名:`caly-coreconf` / `caly-corectl` /
`caly-profile` / `caly-composition` / `caly-dns` / `caly-subscription` /
`caly-template`;`caly-cores` 名弃用(距 `<proj>-core` 铁律一个字母)。

| 提案 | 处置 | 理由/复活条件 |
|---|---|---|
| backends→adapters 等纯审美改名 | 否 | churn 大于增益 |
| ports 并入 domain/application | 否 | M4 成环实证 |
| caly-fetch 微 crate | 否 | profile→subscription 声明边已合法 |
| DNS 渲染进 caly-dns(v2 R1) | 否 | §5.1:R2 + E1 分流证成 |
| 子系统两两禁边(v2) | 否 | 禁环+声明边替代 |
| **server → composition**(三轮批评四) | **否** | server 是 driver adapter,消费的 `ApplicationServicePort` 本身就是用例端口;改道会让 composition 沦为 command/event 词汇表的 re-export 瓶颈,decoupling 是幻象。composition 是 build 期接线,职责终于 listen 之前,bins 是其正当唯一宿主。**复活条件**:出现第二个 executable host(测试 harness crate 等)需要已组装 application 时重审。 |
| profile 主类型避碰撞 | 采纳 | 定名 caly-profile;主类型 `AppConfig` 保留不改名;立"禁裸 `Profile` 类型名"约定(domain::Profile 与此 crate 语言不同,文档中 profile crate 恒指 caly-profile) |

---

## 7. 目标依赖图(每边有 §5 实证;无 peer 边起步)

```
domain(0)   ports(1){+协调 cell}   platform(1)
   ↑              ↑                   ↑
dns(1)──┘   subscription(1,+reqwest)  template(2)→platform
   ↑              ↑
coreconf(2)→{domain, dns}           corectl(2)→{domain, platform}
profile(2)→{domain, platform, subscription}
   ↑
backends(3)→{domain, ports, platform, coreconf, corectl, profile, subscription, template, dns}
application(3)→{domain, ports}                      ★收敛(A 实证可行)
composition(4)→{domain, ports, application, backends, coreconf, corectl,
                profile, platform, subscription, template, dns}
protocol(4)→{domain}
server(5)→{domain, application, protocol}   (决议留档,§6)
tui(5)→{domain, protocol}
bins(6): caly→按需全家+composition;template-worker→{domain, template}
```

---

## 8. 制度补丁:P0 落地,预算分期收紧

1. `check-dependency-discipline` 改 metadata 驱动(cargo metadata 推导偏序)。
2. `crate-budgets.toml`:基线=现状实测;**每期出口收紧一次**,收紧必须
   附理由注释:

| crate | P0 基线 | P2 后 | P3a 后 | P6 后 | P7 后 |
|---|---:|---:|---:|---:|---:|
| caly-profile | 14 000 | ≤13 100 | ≤10 500 | ≤7 500 | ≤7 500 |
| caly-template | — | ≤950 | ≤950 | ≤950 | ≤950 |
| caly-coreconf | — | — | ≤3 100 | ≤3 100 | ≤3 100 |
| caly-corectl | 3 900 | — | ≤3 400 | ≤3 400 | ≤3 400 |
| caly-dns | — | — | — | ≤2 200(P4 出生线) | — |
| caly-subscription | — | — | — | ≤4 100(出生线) | — |
| caly-application | 9 300 | — | — | — | ≤7 000 |
| caly-composition | — | — | — | — | ≤2 800(出生线) |

("—"=不适用;出生线=新 crate 落地即带预算。最终数值以 P0 落盘的
crate-budgets.toml 为唯一权威。)

---

## 9. 迁移分期(P0→P7;机械改名与语义搬迁永不同期)

| 期 | 内容 | 关键出口 |
|---|---|---|
| P0 | 纪律脚本 metadata 化;crate-budgets.toml 基线 | 6 脚本绿 |
| **P1a** | **kernel→corectl 纯改名**(含 ::ports→::contract,纯标识符);零内容移动 | diff 只含路径/import |
| **P1b** | **config→profile 纯改名**;只动模块路径与 manifest,用户面字符串(config.d、日志文案)一律不碰 | 同上 |
| P2 | caly-template 拆出;worker bin 脱 reqwest 链 | worker deps == {domain, template} |
| **P3a** | **coreconf 建立=纯搬迁**:golden 测试先行;E1 三向分流(渲染文件+词辈类型入 coreconf;融合便利 fn 入 backends);mihomo publish 半份入 backends;M9 注册表合一 | **字节零 diff,golden 全绿** |
| **P3b** | typed model 化(serde struct 取代 `json!` 缝合);允许 golden 更新,**每处 diff 附语义等价论证** | golden diff 全部有编号论证 |
| P4 | caly-dns 收容(domain 三文件+platform probe+wire);结构化 Nameserver 落地;coreconf DNS 渲染**只换输入类型**,不搬文件 | 渲染字节仍零 diff |
| P5 | **B1–B6 逐条修复 + F1**,一编号一 commit 一测试 | ≥8 新测试,diff 挂编号 |
| P6 | caly-subscription 拆出(纯 intake+net);reqwest 换标;#75 注释改写 | profile 无 reqwest |
| P7 | composition 抽离;两 cell 上移 ports;application deps 收敛 | application deps == {domain, ports} |

每期门禁:check --workspace --all-targets → clippy 0 警告 → workspace 全量
测试(JOBS=1,DEBUG=0)→ 6 纪律脚本 → fmt --check;**预算闸门随期收紧**
(§8 表)。基线 1028 passed / 0 failed / 39 suites,不破不进下一期。

---

## 10. 验收标准(逐条可审)

1. 15 crates + 2 bins;cargo metadata 实审 == §7 坐标,无环、无未声明
   peer 边;"kernel"一词仅存历史注释;
2. 6 纪律脚本全绿且 metadata 驱动;预算闸门生效(任一超线即红);
3. **B1** coreconf 保 DoH path 渲染测试;**B2** `8.8.8.8:53` 判 IP 且可
   当选 domain_resolver 测试;**B3** scheme 感知 probe 分派测试;
   **B4** mihomo direct-nameserver + sing-box direct 首选 resolver 测试;
   **B5** `999.1.1.1/99` 拒绝测试;**B6** listen 单一家校验测试;
4. coreconf deps == {domain, dns}(+serde 外部);corectl deps ==
   {domain, platform};**无 peer 边**;
5. application deps == {domain, ports};application src 内非 domain/ports
   引用面 == 0;
6. profile 无 reqwest;worker bin deps == {domain, template};
7. 全量测试 ≥ 1028 基线 0 失败;clippy 0 警告;fmt 净;
8. 双二进制无尺寸回归(caly ≤ 6.26 MiB,worker ≤ 341 KiB)。

---

## 11. 风险与回退

- **R1 改名半径**(P1a/P1b):一次性 sed,当期门禁终审;整期还原回退。
  P1b 的 sed 模式白名单化(只匹配 `caly_config`/`caly-config`/manifest
  name),用户面字符串零触碰,宁可漏改不可误改。
- **R2 渲染字节漂移**(P3a):golden 先行;P3a 出口字节零 diff;P3b 的每处
  diff 挂语义等价论证,铁律二保护可 bisect。
- **R3 P7 接口凝结**:composition 对外形状抄现有 ApplicationComposition/
  RunningApplication 签名,不改语义。
- **R4 沙箱工具链脆性**:rustup 重装 + cc-1.4.0 清理手册常备;不升级依赖。
- **R5 预算闸门误报**:基线实测+分期收紧;调整走文件注释,不口头豁免。
