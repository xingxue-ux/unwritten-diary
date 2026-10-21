// 设置的数据层（契约 4.4 的 `settings.get` / `settings.update`）。
//
// 这一层**不碰 UI**：不 import 任何 ArkUI 组件，页面只把这里公开的字段镜像到自己的
// `@State` 上。对应 Windows 版的 `index_settings_controller.dart` /
// `style_settings_controller.dart` 那类 controller——逻辑在 controller，皮肤留给
// panel（鸿蒙的 panel 等 UI 统一那一步再做）。
//
// 这一层只做四件事：
// 1. `refresh()`：读 `settings.get`，失败时把核心原话留在 `error` 里（不假装成功）；
// 2. `update(patch)`：部分更新，带 `expected_revision`（乐观锁）；
// 3. **三态**（`default_style_id`）：省略 = 不改、显式 `null` = 清空、显式值 = 设置。
//    唯一「省略」的出口是 [`buildSettingsInput`]——patch 里没有这个键时，生成的
//    `SettingsInput` **根本不含这个字段**，`JSON.stringify` 会把它整个省掉。
//    **不要**把三态拍平成 `string | null`：那会让「没改」变成「清空」，静默丢掉
//    用户设好的默认风格（#162 复审抓到的 bug）。
// 4. 冲突：`revision_conflict` 时**自动重读**，并如实说明「这次没保存、已刷新」，
//    绝不拿旧 revision 静默覆盖别人写进去的值。

import { Settings, SettingsInput, UpdateSettingsParams } from '../core/Models';
import { CoreGateway, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';

/**
 * 部分更新：只写想改的字段。
 *
 * 没写的字段保持当前值（调用方不用为了「只改开关」把整份设置搬来搬去）。
 *
 * `default_style_id` 的三态直接沿用「键在不在」：
 * - **不写这个键** = 不改；
 * - 写 `null` = 清空；
 * - 写字符串 = 设置。
 *
 * 写成 `undefined` 与「不写」等价（`JSON.stringify` 同样会省略）——**不要**用
 * `undefined` 表达清空，清空只能写 `null`（见 [`SettingsStore.clearDefaultStyle`]）。
 */
export interface SettingsPatch {
  diary_time_zone?: string;
  auto_organize_enabled?: boolean;
  auto_organize_time?: string;
  default_style_id?: string | null;
}

/**
 * 把「当前设置 + patch」拼成 `settings.update` 的入参（三态的唯一出口）。
 *
 * 三个必填字段没给就用当前值；`default_style_id` 只有 patch **显式带了**才写——
 * 这是「省略 = 不改」的全部实现。显式 `null` / 显式字符串原样带下去。
 */
export function buildSettingsInput(current: Settings, patch: SettingsPatch): SettingsInput {
  const input: SettingsInput = {
    diary_time_zone: patch.diary_time_zone === undefined
      ? current.diary_time_zone
      : patch.diary_time_zone,
    auto_organize_enabled: patch.auto_organize_enabled === undefined
      ? current.auto_organize_enabled
      : patch.auto_organize_enabled,
    auto_organize_time: patch.auto_organize_time === undefined
      ? current.auto_organize_time
      : patch.auto_organize_time,
  };
  // 只有调用方**显式**提到这个字段时才碰它。写成 `if (patch.default_style_id !== undefined)`
  // 而不是 `input.default_style_id = patch.default_style_id ?? null`：后者会把「没改」
  // 变成「清空」，正是 #162 复审抓到的那个 bug。
  if (patch.default_style_id !== undefined) {
    input.default_style_id = patch.default_style_id;
  }
  return input;
}

/**
 * 设置状态容器：可被任意页面复用。
 *
 * 用法（页面侧）：
 * ```ts
 * const store = new SettingsStore(dbPath);
 * await store.refresh();               // 读
 * await store.setAutoOrganizeEnabled(true);  // 写（内部带 expected_revision）
 * this.settings = store.settings;      // 页面自己镜像到 @State
 * this.settingsError = store.error;
 * ```
 *
 * `update` 与 `refresh` 是串行的：`loading` 为 true 时新的调用直接返回 `false`，
 * 不排队、不重放（写入不是幂等操作，排队反而会拿旧 revision 连发）。
 */
export class SettingsStore {
  /** 当前设置；`null` = 还没读到。 */
  settings: Settings | null = null;
  /** 当前行的乐观锁版本（= `settings.revision`；没读到时是 0）。 */
  revision: number = 0;
  /** 正在读或写。 */
  loading: boolean = false;
  /** 读/写失败时核心或宿主的原话（成功时是空哨兵）。 */
  error: UiError = EMPTY_UI_ERROR;
  /** 最近一次写的结果说明：成功、冲突后已刷新、本地拒绝……没有提示时是空串。 */
  message: string = '';
  /** 最近一次写是否因为 `revision` 过期被拒（为 true 时 store 已经自动重读过）。 */
  conflicted: boolean = false;

  private dbPath: string = '';

  constructor(dbPath: string) {
    this.dbPath = dbPath;
  }

  /** 读当前设置。读失败时保留上一次成功读到的值，并把原因写进 `error`。 */
  async refresh(): Promise<boolean> {
    if (this.loading) {
      return false;
    }
    this.loading = true;
    this.message = '';
    this.conflicted = false;
    const ok: boolean = await this.read();
    this.loading = false;
    return ok;
  }

  /**
   * 部分更新：`expected_revision` 用当前读到的 `revision`。
   *
   * - 成功：把核心返回的新对象收下（`revision` 随之前进），返回 `true`；
   * - `revision_conflict`：**重读**并置 `conflicted`，返回 `false`，`message` 说明
   *   「设置已被改动，已刷新」——不覆盖别人写进去的值；
   * - 其它失败：重读一次让状态回到存储里的真实值，再把错误留在 `error`，返回 `false`。
   */
  async update(patch: SettingsPatch): Promise<boolean> {
    const current: Settings | null = this.settings;
    if (current === null || this.loading) {
      return false;
    }
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const params: UpdateSettingsParams = {
      settings: buildSettingsInput(current, patch),
      expected_revision: current.revision,
    };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'settings.update', params);
    if (result.ok) {
      this.loading = false;
      this.apply(result.data as Settings);
      this.message = '已保存';
      return true;
    }
    const error: UiError = uiErrorOf(result);
    if (error.code === 'revision_conflict') {
      this.conflicted = true;
      const refreshed: boolean = await this.read();
      this.loading = false;
      this.message = refreshed
        ? '设置已被改动，已刷新到最新版本；请确认后再改一次。'
        : '设置已被改动，但重读也失败了；上面的错误说明了原因。';
      return false;
    }
    // 其它失败：先重读（界面回到真实存储值），再挂错误，免得读成功把错误清掉。
    await this.read();
    this.loading = false;
    this.error = error;
    this.message = '这次修改没有保存。';
    return false;
  }

  /** 只改自动整理开关；默认风格不动（省略）。 */
  setAutoOrganizeEnabled(enabled: boolean): Promise<boolean> {
    const patch: SettingsPatch = { auto_organize_enabled: enabled };
    return this.update(patch);
  }

  /** 只改计划时间（`HH:mm`）；默认风格不动（省略）。 */
  setAutoOrganizeTime(time: string): Promise<boolean> {
    const patch: SettingsPatch = { auto_organize_time: time };
    return this.update(patch);
  }

  /** 只改日记时区（IANA 名）；默认风格不动（省略）。 */
  setDiaryTimeZone(zone: string): Promise<boolean> {
    const patch: SettingsPatch = { diary_time_zone: zone };
    return this.update(patch);
  }

  /**
   * 设置默认风格（三态里的「显式字符串」）。
   *
   * 空串/纯空白**拒绝**：核心会把空串当成「没有默认风格」，也就是清空——那不是
   * 「设置」。要清空请显式调用 [`clearDefaultStyle`]。
   */
  async setDefaultStyle(styleId: string): Promise<boolean> {
    const trimmed: string = styleId.trim();
    if (trimmed.length === 0) {
      this.message = '默认风格 id 不能为空；要清空请用「清空默认风格」。';
      return false;
    }
    const patch: SettingsPatch = { default_style_id: trimmed };
    return this.update(patch);
  }

  /**
   * 清空默认风格（三态里的「显式 `null`」）。
   *
   * **全工程唯一**给 `default_style_id` 赋 `null` 的地方。
   */
  clearDefaultStyle(): Promise<boolean> {
    const patch: SettingsPatch = { default_style_id: null };
    return this.update(patch);
  }

  private async read(): Promise<boolean> {
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'settings.get', null);
    if (result.ok) {
      this.apply(result.data as Settings);
      this.error = EMPTY_UI_ERROR;
      return true;
    }
    this.error = uiErrorOf(result);
    return false;
  }

  private apply(saved: Settings): void {
    this.settings = saved;
    this.revision = saved.revision;
  }
}