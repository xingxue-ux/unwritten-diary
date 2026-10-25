# 鸿蒙端 UI 交接说明

> 本文记录 UI 补齐前的历史交接状态。2026-10-22 已在此基础上实现 Windows 风格界面，
> 当前实现与验证边界见[鸿蒙 UI 对齐与验收](鸿蒙UI对齐与验收.md)。下文“未做到”清单保留作历史对照。

> **一句话**：C ABI、数据层、契约文档是**稳定层**，请在其上重做 UI；`pages/`、`ui/Sketch.ets`、`Index.ets` 的导航是**临时层**，可以整体重写。
>
> 交接时间：2026-10-22。交接触发原因：产品决定「UI 与 Windows 版保持一致」，由对方重做。

---

## 1 为什么这样切分

重做 UI 最贵的地方不是写页面，是**把埋在页面里的业务判定挖出来**。所以这轮把所有判定都推到了 `state/*.ets`（数据层），页面只做「调 store + 显示状态 + 点按钮」。

结果是：

| 层 | 位置 | 你重做 UI 时 |
|---|---|---|
| **稳定层** | `crates/`（核心）、`crates/diary_capi`（C ABI 59 条）、`entry/src/main/ets/state/*.ets`（五个 store） | **不用动**，直接用 |
| **协议层** | `docs/development/01-共享接口契约.md`、`docs/architecture/m6-核心-C-ABI.md` | 只读；发现不一致请改文档并留证据 |
| **临时层** | `entry/src/main/ets/pages/*.ets`、`ui/Sketch.ets`、`Index.ets` 导航条 | **可以整体重写** |

`pages/*.ets` 里每个新写的页面文件头都有这段注释，看到就说明它可以被替换：

```
// ⚠️ 临时功能界面：UI 将由对方按 Windows 版重做。
// 本文件只做「把 XxxStore 的状态显示出来 + 把按钮点下去」。
// 请勿在此加入业务逻辑——那会让重做 UI 时不得不搬逻辑。
```

---

## 2 数据层（稳定，直接用）

五个 store，**纯 ArkTS 数据层，不 import 任何 ArkUI 组件**：

| 文件 | 覆盖的 C ABI | 关键语义（**重做 UI 也必须守住**） |
|---|---|---|
| `state/DiaryStore.ets` | `diary.listDays` / `getDay` / `generate` / `setCurrent` / `jobs.runDue` / `jobs.status` | **票据 ≠ 已生成**；自动整理结果**一律是候选**；`waiting_configuration` **不是失败** |
| `state/RecycleStore.ets` | `captures.list` / `restore` / `previewPurge` / `purge` | `captures.list` **不按状态过滤**，客户端自己筛；`purge` **必须带 preview 令牌**；三类版本清单**分开显示** |
| `state/StyleStore.ets` | `styles.list` / `get` / `save` | `expected_revision` **可选**（省略=明确接受覆盖）；冲突后重读，**不拿旧 revision 重试** |
| `state/ExportStore.ets` | `exports.create` | **同步**返回；`jobId` 指向已 `succeeded` 的任务；`restorable` 恒 `false`（**不是备份**） |
| `state/SettingsStore.ets` | `settings.get` / `update` | `default_style_id` **三态**：省略=不改 / `null`=清空 / 值=设置 |

参考实现（接口形状可对照，Dart 细节不要照抄）：`apps/diary_app/lib/src/{library_care,style_settings,readable_export,ai_diary,index_settings}_controller.dart`。

---

## 3 产品口径（比「好看」优先的几条）

这几条不是风格问题，是**「界面不许说谎」**：

1. **候选 ≠ 当前**。自动整理生成的版本是**候选**，`current_version_id` 仍是 `NULL`。界面必须把「候选版本（未采用）」和「当前版本」**分开显示**，不能把候选写成「已生成日记」。
2. **没配模型 ≠ 生成失败**。任务停在 `waiting_configuration`（`requires_user_action=true`），要显示「还没配置模型服务，配置好后任务会自动继续」，**不要显示成红色错误**。
3. **预览清单的四类必须分开**：
   - ① `diary_versions_still_referencing`（开关打开才删）
   - ② `derived_diary_version_ids`（**不删**）
   - ③ `undetermined_diary_version_ids`（**判不出归属，不删**）
   - ④ `retained_source_snapshot_ids`（不删）

   合并成一句「有 N 个相关版本」会让用户以为都删了——**这是界面说谎，比丑严重**。
4. **`exports.create` 不是备份**。它导出的是已生成的正文，`restorable` 恒 `false`，界面上不要说成「备份」。
5. **回收站里的记录不会从列表消失**（`captures.list` 不过滤 `state`），主列表要自己排除 `trashed`。

---

## 4 与 Windows 版的对照（已知差异）

视觉令牌在 `ui/Sketch.ets`（**可整体重写**），值与 `apps/diary_app/lib/src/sketch_ui.dart` 的 `SketchColors` 逐字符一致：

```
canvas #F8F7F2   paper #FFFEFA   soft #F1F0EA   button #EAE8E0
ink    #303331   text  #454541   muted #62645E  line #D8D9D2   blush #D9C1B3
```

**这一版没做到、需要你补齐的**：

| # | 差异 | 说明 |
|---|---|---|
| 1 | **手绘抖动描边** | Windows `SketchFrame` 用 `sin` 采样抖动 + 16% 重影线；ArkTS 版是 `line` 实线直角（要用 `Canvas` 逐点画，代价大，故意留给 UI 重做） |
| 2 | **字体** | Windows 用 `DiaryDisplay` / 微软雅黑；鸿蒙走系统 HarmonyOS Sans，**字号/字重/颜色一致、字形不同**。要完全一致需把 `DiaryDisplay-AppSubset.ttf`（217 KB，OFL 许可）打包进 `resources` |
| 3 | **猫头标记** | Windows 最左有手绘猫头，鸿蒙没有对应资源 |
| 4 | **任务入口** | Windows 是铃铛图标，鸿蒙是文字「任务」 |
| 5 | **顶栏高度/字号** | 鸿蒙 76vp / 导航 15fp；Windows 约 86vp / Material TextButton ≈14fp |
| 6 | **页面内部** | 本版页面内部仍是系统白底、圆角、蓝色按钮——外壳改了，**内部没改** |
| 7 | **分隔线** | 本版用 `line` 全宽；Windows 用 `ink` 且左右缩进 24 |
| 8 | **布局密度** | 本版是单列堆叠，PC 大屏右侧全空；Windows 有主从/分栏 |

---

## 5 当前页面完成度

| 页面 | 状态 |
|---|---|
| 记录 / 搜索 / 片段 | 既有实现（未接 `Sketch.ets`，仍是系统视觉） |
| **日记** | 已接 `DiaryStore`：天列表、当天材料数、当前/候选、生成、候选正文、采用为当前 |
| **回收站** | 已接 `RecycleStore`：列表、恢复、预览四类清单、开关、令牌过期如实提示 |
| **我的 → 存储与回收站** | 已可进入回收站 |
| 风格 / 导出 | **只有 store 与 C ABI，没有页面**（`Index.ets` 里 `styles` / `export` 是占位路由） |
| 任务中心 | 既有实现 |

---

## 6 验证怎么做

```bash
# 静态检查（CI 也跑这些）
python3 tools/check_arkts_scope.py        # .ets 当 .ts 交给 tsc，抓语法/作用域错误
python3 tools/check_arkts_capi_contract.py # C ABI 59 条 ↔ DiaryCore.ets ↔ 文档三者一致
python3 tools/check_ui_parity.py          # 两端五入口 + 共享文案 key（支持顶部导航与底部页签两种结构）
python3 tools/check_docs.py

# 真机（ASCII 路径下构建，中文路径 hvigor 会拒绝）
cd apps/ohos-diary-core
OHOS_BUNDLE_NAME=com.example.myapplication SKIP_ASR_LIBS=1 bash scripts/build-hap.sh
```

**`check_arkts_scope.py` 只覆盖 `pages/` 与 `ui/` 之外的文件**——那两处用 ArkUI 尾随闭包语法，`tsc` 解析不了。**改这两个目录必须本地跑 `build-hap.sh`**（CI 不做 ArkTS 编译，鸿蒙 SDK 装不进 runner）。

---

## 7 踩过的坑（省你时间）

1. **`hvigorw` 拒绝含中文的项目路径**：必须在 ASCII 路径下构建，否则报 `Please modify the project path…`。仓库主目录 `不写日记` 就中招，得用 worktree。
2. **`hdc file send` 写不进应用沙箱**（SELinux）：模型/配置要用 `hdc rport` + 宿主 HTTP 服务，让应用自己下载。
3. **`hdc install -r` 可能"返回成功但仍跑旧包"**：用 `bm install -r -p /data/local/tmp/x.hap -u 100` 更可靠。
4. **不要用 `sqlite3` 就地打开设备上的库**：shell 身份会留下 `-wal/-shm`，应用随后报 `attempt to write a readonly database`。要读就 `hdc file recv` 拷出来再读。
5. **「扬声器播 wav + 麦克风录回来」不能验转写准确率**：系统回声消除会把自己正在播放的音频从麦克风输入里减掉，转写结果是幻觉。要验准确率就注入已知 PCM（`recordings.registerSegment` → `finalize`），实测相似度 1.0000（`verify_mode asr_inject`）。
6. **跨调用共享核心会话**：`preview_token` 只活在核心会话内存里。`CoreWorker` 若每次调用都 `open → close`，`previewPurge` 之后 `purge` 必报令牌过期。修复见 PR（保持按 `dbPath` 缓存句柄）。

---

## 8 判定标准

**「界面能用」不等于「界面诚实」。** 本项目对后者的要求高于前者：

- 拿不到的数据要说「还没有」，不能显示 0；
- 候选版本不能写成已生成；
- 没能删除的版本要列出来，不能合并进「已清理」；
- 没跑过的验证不许写「已验证」。

`CONTRIBUTING.md` §10.1.1 有完整的「验过了的四层含义」（本地 / 干净 checkout / CI / 真机），提 PR 时按那四层写验证，**没跑的那层明确写「未验」**。
