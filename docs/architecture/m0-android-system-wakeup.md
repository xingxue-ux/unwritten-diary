# Android 系统唤醒接入（#19）

应用启动时注册 WorkManager 的 15 分钟周期保底任务。核心打开后，前台还会读取 `jobs.nextWakeup`，登记一次性任务；文字提交等核心状态变化后重新登记。系统在应用不在前台时启动独立 Flutter isolate，该入口重新打开同一资料库并调用 `jobs.runDue(budgetMs: 20000, trigger: wake)`。成功或失败都写入应用私有目录 `unwritten_diary/wakeup-diagnostics.jsonl`；失败由 WorkManager 重试，下一次唤醒仍以核心返回的 `nextWakeup` 为准。

两个交替的一次性任务名避免工作者在执行中重新登记自己时取消当前任务。周期任务用于拾起尚未来得及登记的导入或系统分享任务。`initialDelay` 只是最早执行时刻，不承诺准点：Doze、省电策略、系统负载都可能推迟。结果日志中的 `expectedAt`、`observedAt`、`completedAt` 用于量测实际偏差，`processed`、`succeeded`、`failed`、`retrying` 等数字直接来自核心返回，不是界面推断。

## 真机验收记录模板

1. 安装含 `libdiary_bridge.so` 的 Android 包，创建能排入任务队列的记录/导入。
2. 退出前台，观察 `adb shell dumpsys jobscheduler` 中本应用的 WorkManager 任务；等待系统实际唤醒。
3. 取出应用私有目录中的 `wakeup-diagnostics.jsonl`，按 `observedAt - expectedAt` 计算偏差；核对核心任务状态确实改变。
4. 重复锁屏、开启省电模式、杀死应用进程后的实验，记录推迟与恢复行为。系统「强行停止」应用后，Android 不保证继续分发任务，需要再次手动打开应用。

当前实现已具备入口与诊断数据，以上真机量测需在 Android 设备完成，不能以桌面 Flutter 测试代替。

参考：[Android WorkManager](https://developer.android.com/develop/background-work/background-tasks/persistent/getting-started)、[Flutter Workmanager](https://pub.dev/packages/workmanager)。
