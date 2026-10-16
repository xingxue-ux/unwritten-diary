# Android 系统唤醒接入（#19）

应用启动时注册 WorkManager 的 15 分钟周期保底任务。核心打开后，前台还会读取 `jobs.nextWakeup`，登记一次性任务；文字提交等核心状态变化后重新登记。系统在应用不在前台时启动独立 Flutter isolate，该入口取得同一资料库的进程内共享 `BridgeSession` 并调用 `jobs.runDue(budgetMs: 20000, trigger: wake)`。共享锁阻止后台任务与前台导入并发使用两个 `Core`，也避免第二次 `Core::open` 把正在复制的导入误作中断恢复。只有最后一个会话释放后，下一次打开才重新执行恢复。运行与异常写入应用私有目录 `unwritten_diary/wakeup-diagnostics.jsonl`；临时失败由 WorkManager 重试，永久性契约错误不重试。

一次性任务统一使用 `diary-core-wakeup` 唯一名，新任务追加到正在执行的工作后面，避免取消当前工作；升级时取消旧版两个交替任务。周期任务用于拾起尚未来得及登记的导入或系统分享任务。周期任务和一次性任务仍可能同时启动，但共享的 `Core` 锁会串行处理。该锁只覆盖同一个 Android 进程；应用不得另开进程直接访问同一资料库。`initialDelay` 只是最早执行时刻，不承诺准点：Doze、省电策略、系统负载都可能推迟。到期时间已经过去时，最少等待 15 秒以免立即循环。20 秒预算在两条核心任务之间判断，不会强行中断单条长任务。结果日志中的 `expectedAt`、`observedAt`、`completedAt` 用于量测实际偏差，`processed`、`succeeded`、`failed`、`retrying` 等数字直接来自核心返回，不是界面推断。

## 真机验收记录模板

1. 安装含 `libdiary_bridge.so` 的 Android 包，创建能排入任务队列的记录/导入。
2. 退出前台，观察 `adb shell dumpsys jobscheduler` 中本应用的 WorkManager 任务；等待系统实际唤醒。
3. 取出应用私有目录中的 `wakeup-diagnostics.jsonl`，按 `observedAt - expectedAt` 计算偏差；核对核心任务状态确实改变。
4. 重复锁屏、开启省电模式、杀死应用进程后的实验，记录推迟与恢复行为。系统「强行停止」应用后，Android 不保证继续分发任务，需要再次手动打开应用。

当前实现已具备入口与诊断数据，以上真机量测需在 Android 设备完成，不能以桌面 Flutter 测试代替。

参考：[Android WorkManager](https://developer.android.com/develop/background-work/background-tasks/persistent/getting-started)、[Flutter Workmanager](https://pub.dev/packages/workmanager)。
