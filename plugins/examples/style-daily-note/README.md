# 示例：日常风格模板（声明式）

纯数据插件：没有 `main.wasm`，只有 `manifest.json` + `main.json`。宿主按白名单
占位符把 #114 的 `StyleProfile` 字段渲染成一段中文风格说明；模板不会改写历史日记，
也没有任何权限。

## 安装与调用

```text
plugins.inspectPackage(path, "linux")   → 清单/模板/权限/兼容性（不执行内容）
plugins.install(path, "linux", [])      → 启用
plugins.invoke({
  plugin_id: "example.style-daily-note",
  activation: "organize.template.select",
  template_id: "style.daily-note",
  style_profile: { ... StyleProfile 的字段 ... }
})
```

输出示例（字段为空时对应位置为空）：

```text
写作偏好（用户确认）：
- 篇幅：三百字以内
- 叙述人称：第一人称
- 语气：平静口语
- 情绪温度：不煽情
- 修辞程度：少比喻
- 参考示例：今天下雨。
- 禁用表达：综上所述
```

`command:style` 也声明了，可以用用户命令触发同一模板。模板只做**单遍替换**：
示例文本里的 `{{...}}` 不会被再次展开。

## 检查

```bash
python3 plugins/tools/check_package.py plugins/examples/style-daily-note
cargo run -p plugin_pack -- check plugins/examples/style-daily-note
```
