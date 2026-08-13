# P8 设计:caly-cli 表现层抽取与边界缝合(v5 草案)

> 状态:**设计草案(待评审,未实施)**。日期:2026-08-13。姊妹文档:`docs/crate-replan.md`(v4 定稿)、`docs/cli-v3-design.md`。
>
> 触发条件(v4 §5.7 原文):"CLI v2 落地期必须重估(bins 内 lib 化或 **caly-cli**)"
> ——**CLI v3 已落地**(cli-v3-design.md,2026-08-13),M7 的延期条件成熟,本条即为重估结论。

---

## 1. 现状证据(2026-08-13 实测,post-P7 + 收敛轮后)

### 1.1 bins/caly 单体型(32,346 行,全 workspace 之最)

| 面 | 模块 | 行数(含测试) |
| --- | ---: | ---: |
| **daemon 宿主** | daemon.rs 481 / daemon_config 566+587 / doctor 249+361+98 / dns 260 / subscription(refresh) 647 / logging 100 / bootstrap 143 / main 111 / error ~300 / mock_kernel 314 / 宿主测试 49 | **≈ 4,266** |
| **CLI 面** | cli/ 语法+别名 2,590+2,242t / client/ RPC+离线操作 ≈11.5K / commands/ 分派+视图 ≈4.5K / entry_tree 900+615t / output 794 / error/ 命令错误域 | **≈ 28,080** |

### 1.2 CLI 面真实依赖面(全量 `use caly_*` 统计)

| crate | 次数 | 层序 |
| --- | ---: | --- |
| caly_platform | 21 | 1 ✓ |
| caly_profile | 15 | 2 ✓ |
| caly_protocol | 8 | 4 ✓ |
| caly_domain | 4 | 0 ✓ |
| caly_corectl | 4 | 2 ✓ |
| caly_subscription | 1 | 1 ✓ |
| **caly_backends** | **1 use + 5 内联 = 6 处** | **❌ 偷渡** |

### 1.3 偷渡面穷举(仅 2 个符号)

| 符号 | 位置 | 性质 | 归位 |
| --- | --- | --- | --- |
| `subscription_id_for_url` | 定义:backends/subscription/http.rs:568;消费:client/subscription.rs:586、sources.rs:233、tests.rs:690、set/sub.rs:413、entry_tree.rs:311 | URL→SubscriptionId 稳定哈希,**intake 域函数**,非 Port 实现 | **→ caly-subscription**(capability 1,CLI 合法可依赖) |
| `LinuxSystemProxyBackend::new` | 消费:commands/sys_status.rs:24,96;doctor/checks.rs(可用性项) | 桌面会话探测 = **系统触点**,属 platform 所有权(M12 精神) | **探测面 → caly-platform**(desktop 检测);gnome/kde/niri 完整 Port 实现(apply/restore)**留在 backends** |

### 1.4 反证:为何不是其他候选

- backends 的 core.rs 689 / dual.rs 322 / lifecycle.rs 554:**合法 Backend**(Port 实现,imports 实测仅 {ports, domain, corectl, platform})——不是错位,不动;
- caly-profile 6,579:schema/loader/store 内聚,≤7,500 预算 ✓ 不动;
- application 5,744:已收敛 deps=={domain,ports} ✓ 不动;
- server→composition 否决(v4 §6):复活条件"出现第二个 executable host"**未满足**——bins/caly 仍是唯一 bin,不重审。

---

## 2. 目标架构:caly-cli(role 轴, layer 5)= 表现层复活

v4 层模型里 layer 5 曾有 tui(5)→{domain, protocol};TUI 死后该层空缺,
CLI 客户端(RPC + 离线投影)是它的语义继任者。P8 落地:

```
caly-cli(5, role)  →  {domain, ports, platform, profile, coreconf,
                        corectl, subscription, protocol}   # 实测面,全部合法
                    禁 {application, composition, backends, server}
bins/caly(6)       →  daemon 宿主(daemon_config/doctor/refresh/dns/logging)
                      + main + caly-cli(高→低 ✓)
```

- 边界 = **crate 编译边界 + 纪律脚本禁令**:表现层永不触碰组装/驱动层,
  UDS wire(protocol v2)是两进程唯一通道——"thin client" 铁律以 crate 形态复活;
- `caly daemon`(前台宿主)由 main() 拦截:`caly_cli::run` 返回
  `Outcome::{HostRun(HostArgs), Exit(code)}`,host 执行留在 bin——
  维持 v4 §6 "composition 的正当唯一宿主是 bins" 决议;
- e2e 测试(bins/caly/tests/,经 CARGO_BIN_EXE 与 UDS 测二进制行为)不随迁;
  `cli/cli_tests.rs` 等 src 内测试随迁。

---

## 3. 分期(机械搬迁与语义缝合永不同期;一 commit 一期)

| 期 | 内容 | 关键出口 |
| --- | --- | --- |
| **P8a** | **纯搬迁**:git mv cli/ client/ commands/ entry_tree/ output.rs error/ 等 → crates/caly-cli(新建 lib,metadata 声明 axis=role layer=5);bins/caly 仅留宿主面;main 接 Outcome 分派;test_helpers 随迁(宿主测试所需 temp-root helper 以 pub test-support 模块暴露) | **字节零 diff,全量测试绿(392+)** |
| **P8b** | **边界缝合**(2 符号):`subscription_id_for_url` 迁 caly-subscription(backends 改指,保 re-export 兼容);sysproxy 桌面探测上移 caly-platform(gnome/kde/niri 消费探测结果),sys_status/doctor 改指 platform;每处 diff 附语义等价论证 | cli 侧 `use caly_backends` == **0** |
| **P8c** | **制度**:check-dependency-discipline 收编 caly-cli 声明;check-integration-invariants 增"caly-cli 禁 {application, composition, backends, server} 生产边";check-presentation-invariants 路径改指 crates/caly-cli/;crate-budgets.toml 加 P8 行(下表);docs 更新(replan v5 本文件转正) | check-all 7/7;预算闸门生效 |

每期门禁:check --workspace --all-targets → clippy 0 警告 → 全量测试
(JOBS=1)→ check-all.sh 全绿 → fmt --check。**结构变更与 bug 修复永不同
commit**(铁律二)。

---

## 4. 预算表增量(P8 行)

| crate | P7 后 | P8a 后 | P8b 后 | P8 定值 | 依据 |
| --- | ---: | ---: | ---: | ---: | --- |
| bins/caly | 34,000 | ≈4.3K | ≈4.3K | **≤5,000** | 壳层,实测+15% 余量 |
| caly-cli | — | ≈28.1K | ≈28.1K | **≤29,500**(出生线) | 实测+~5% |
| caly-subscription | 3,600 | 3,600 | +40(id 函数) | **≤3,700** | 实测+余量 |
| caly-platform | 5,110 | 5,110 | +探测面(≈120) | **≤5,300** | 实测+余量 |
| caly-backends | 9,980 | 9,980 | −id 函数−探测面 | **≤9,800** | 收紧 |

---

## 5. 验收标准(增量条目,并入 v4 §10)

1. cargo metadata 实审:caly-cli 边 == §2 声明面,**无 {application, composition, backends, server} 边**;
2. bins/caly src ≤ 5,000;`use caly_backends` 在 crates/caly-cli 内 == 0;
3. `caly daemon`/`caly doctor`/`caly sysproxy status`/`caly sub list` 行为与文案逐条同前(冒烟脚本比对);
4. 双二进制无尺寸回归(caly ≤ 6.26 MiB,worker ≤ 341 KiB);
5. 全量测试 ≥ 392 基线 0 失败;check-all 7/7;clippy 0 警告;fmt 净。

---

## 6. 风险与回退

- **R1 搬迁半径**(P8a):git mv 一次性;当期门禁终审,整期还原回退(单 commit);
- **R2 测试引用**:cli_tests/doctor_tests 内 `super::`/`crate::` 路径随迁改写(机械);
  e2e 不 import src 内部(已验证走二进制+UDS),不受影响;
- **R3 缝合行为风险**(P8b):sysproxy 探测上移须保 doctor 文案与 sys_status
  输出一致;golden 测试先行比对;
- **R4 宿主测试 helper**:daemon_config_tests(587)依赖 temp-root helper,
  caly-cli 暴露 pub test-support 模块(禁进生产路径),或宿主侧保留 50 行副本——选前者;
- R5(v4 遗留)预算闸门误报:调整走文件注释,不口头豁免。
