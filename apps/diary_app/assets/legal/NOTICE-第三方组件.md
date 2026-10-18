# 第三方组件与许可声明（随包）

本文件与同目录下的许可证全文随安装包分发。它覆盖**实际会进入 Windows / Android 产物的组件**，
不把开发期工具（cargo、Flutter SDK、测试依赖）算作产品组件；开发工具的许可证见仓库与各自发行版。

> 本文件是工程清点，不构成法律意见。标“不确定”的项必须在发布前由负责人确认。

## 1. 应用本体

| 组件 | 版本/来源 | 许可证 | 随包材料 |
|---|---|---|---|
| 不写日记（应用代码） | 本仓库 | MIT | `LICENSE-MIT-app.txt` |

## 2. 模型与原生运行时

| 组件 | 版本/来源 | 许可证 | 随包文件 | 义务/备注 |
|---|---|---|---|---|
| bge-small-zh-v1.5 权重 | BAAI，revision `7999e1d3…`；本仓库自行导出 ONNX | MIT | `LICENSE-MIT-bge.txt` | 保留 MIT 全文与来源；我们分发的是派生 ONNX，不是第三方转换产物 |
| SenseVoice-Small 权重 | FunAudioLLM / ModelScope；sherpa-onnx 转换产物 | MIT（上游仓库） | `LICENSE-MIT-SenseVoice.txt` | 转换产物仓库没有独立许可全文，只指向 FunASR；结论来自上游 LICENSE，**建议发布前复核** |
| sherpa-onnx | v1.13.8 预编译 `libsherpa-onnx-c-api.so` / `.dll` | Apache-2.0 | `LICENSE-Apache-2.0-sherpa-onnx.txt` | 保留许可证；如修改需标注；上游未发现单独 NOTICE 文件（以分发版本为准） |
| ONNX Runtime | 1.29.1，自编（strip 后 19.62 MB） | MIT | `LICENSE-MIT-onnxruntime.txt`、`ONNX-Runtime-ThirdPartyNotices.txt` | 保留 MIT 与 Microsoft 的第三方 notices；自编未改 ORT 源码（构建配方见 m6） |
| ONNX Runtime（sherpa 预编译包内） | 1.28.2 | MIT | 同上 | 若最终只带一份 ORT，仍需保留 ORT notices；若带 sherpa 自带那份，同样适用 |
| libc++ / libc++_shared | Android NDK / OHOS SDK | Apache-2.0 WITH LLVM-exception | `LICENSE-libcxx.txt` | 必须随包；不要删 |
| SQLite | rusqlite `bundled` 静态链接 | Public Domain | 无单独全文 | SQLite 官方声明为 public domain；保留出处说明即可 |

## 3. Flutter / Dart 运行时与插件

Flutter engine 与 Dart 运行时随包，采用 BSD-3-Clause（Flutter 的 `LICENSE` 为准）：

| 组件 | 版本 | 许可证 | 材料 |
|---|---|---|---|
| Flutter engine / Dart | 3.44.6（发布机固定） | BSD-3-Clause | `LICENSE-Flutter-BSD-3-Clause.txt` |
| workmanager | 0.10.10 | MIT | 上游 `LICENSE`（见 `docs/legal/第三方许可与合规.md`） |
| file_selector | 1.1.0 | BSD-3-Clause | 同上 |
| path_provider | 2.1.6 | BSD-3-Clause | 同上 |
| path | 1.9.1 | BSD-3-Clause | 同上 |
| flutter_timezone | 5.1.1 | Apache-2.0 | 同上 |
| 本地包 diary_api / diary_bridge / diary_mock / platform_host | 本仓库 | MIT | `LICENSE-MIT-app.txt` |

传递依赖（`file_selector_*`、`path_provider_*`、`workmanager_*`、`flutter_timezone` 的平台实现等）
也随包；它们的许可证由 `pubspec.lock` 锁定，清单与核对方法见
[`docs/legal/第三方许可与合规.md`](../../../../docs/legal/第三方许可与合规.md)。

## 4. Rust crates

直接依赖：

| crate | 版本 | 许可证 |
|---|---|---|
| `rusqlite` | 0.40.2 | MIT |
| `chrono` | 0.4.x | MIT OR Apache-2.0 |
| `uuid` | 1.x | Apache-2.0 OR MIT |
| `sha2` | 0.10.x | MIT OR Apache-2.0 |
| `encoding_rs` | 0.8.x | Apache-2.0 OR MIT |
| `chardetng` | 1.x | Apache-2.0 OR MIT |
| `zip` | 8.x | MIT |
| `quick-xml` | 0.42 | MIT |
| `pdf-extract` | 0.12 | MIT |
| `jieba-rs` | 0.11 | MIT |
| `serde` / `serde_json` | 1.x | MIT OR Apache-2.0 |
| `thiserror` | 2.x | MIT OR Apache-2.0 |
| `ureq` | 2.12.1 | MIT OR Apache-2.0 |
| `ort` / `ort-sys` | 2.0.0-rc.13 | MIT OR Apache-2.0 |
| `tokenizers` | 0.23.2 | Apache-2.0 |
| `libloading` | 0.9.0 | ISC |
| `flutter_rust_bridge` | 2.13.0 | MIT |

**弱 copyleft 扫描**：`cargo metadata` 的 320 个包中，只有 `r-efi`（5.3.0 / 6.0.0）的 SPDX 是
`MIT OR Apache-2.0 OR LGPL-2.1-or-later`；本项目选择 MIT/Apache-2.0 分支，**不使用 LGPL 分支**。
没有 GPL/AGPL/MPL/EPL/CDDL/SSPL 依赖。完整清单与复查命令见
[`docs/legal/第三方许可与合规.md`](../../../../docs/legal/第三方许可与合规.md)。

## 5. 字体

| 组件 | 来源 | 许可证 | 随包材料 |
|---|---|---|---|
| `DiaryDisplay-AppSubset.ttf` | LXGW WenKai GB + Klee 的子集化改名产物 | SIL OFL 1.1 | `OFL-DiaryDisplay.txt` |

字体合规要点（已用 TrueType `name` 表核对）：

- `nameID 0`（Copyright）保留：`Copyright 2022-2026 LXGW … / Copyright 2020 The Klee Project Authors …`；
- `nameID 13`（License Description）保留：`This Font Software is licensed under the SIL Open Font License, Version 1.1 …`；
- 子集化后改名为 `Diary Display`，没有使用 OFL 保留字体名（`霞鹜`/`霞鶩`/`落霞孤鹜`/`落霞孤鶩`/`LXGW`）；
- OFL 全文随包（`apps/diary_app/assets/legal/OFL-DiaryDisplay.txt`，同时保留 `assets/fonts/OFL.txt`）；
- 不得单独售卖字体；修改版本仍受 OFL 约束；
- **不要把 `nameID 0/13` 当作“可删除的元数据”**。有些子集化工具会清空它们，那会违反 OFL 的
  copyright notice / license 保留要求。

`fc-scan` 看到的 family 是 `Diary Display`，不是原始名；这是改名，不是删除版权声明。

## 6. 应用图标

`apps/diary_app/assets/brand/app_icon.png`（1024×1024 PNG）来源与授权**没有在仓库里记录**。
发布前必须确认它是原创、已购买授权或使用可再分发素材；否则不要随包。**当前标为不确定。**

## 7. 不在本包里的东西

- 用户数据、模型权重文件（模型按 manifest 单独获取/随包时由构建脚本放入，不在此许可证目录）；
- 开发工具（Rust、Flutter SDK、Android SDK/NDK、JDK、hvigor）；
- 服务商 API 的响应内容与商标；
- 任何未在 `Cargo.lock` / `pubspec.lock` 中锁定的东西。

如发布包新增二进制、模型、字体或图标，必须同时更新本文件、`docs/legal/第三方许可与合规.md`
与 `apps/diary_app/pubspec.yaml` 的 `assets/legal/` 声明，并跑
`python3 tools/legal/check_licenses.py`。
