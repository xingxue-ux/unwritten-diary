// 风格偏好的数据层（契约 4.3 的三条：`styles.list` / `styles.get` / `styles.save`）。
//
// 这一层不碰 UI：不 import 任何 ArkUI 组件，页面只把公开字段镜像到自己的 `@State`。
// 风格与 `state/SettingsStore.ets` 一致：字段 + `refresh()` + 写操作，失败把核心原话
// 留在 `error` 里。
//
// 三条硬口径：
//
// 1. **`styles.list` 里一定有默认风格 `sty_default`**（`revision = 1`、内容全空）。它是
//    「没有偏好」的真实配置，不是查不出来的魔法值——所以本 store 不把它藏起来。
// 2. **`styles.get` 对不存在的 id 报 `not_found`，不静默回退默认风格**。本 store 也
//    不回退：`getStyle` 失败时 `current` 置 null，`message` 明说「没有回退」。
//    回退会让界面以为用户设过偏好。
// 3. **`styles.save` 的乐观锁是可选的**：
//    - 省略 `expected_revision` = **明确接受覆盖当前版本**（核心语义）；
//    - 带上 = 冲突检测；不符报 `revision_conflict`，本 store 会**重读最新版本**，
//      绝不拿旧 revision 重试，也不会静默覆盖别人写进去的值。
//    `revision` / `updatedAt` 由核心盖章，调用方传了也不生效；本 store 不再转发。
//
// 每次保存落一个**新的不可变版本**（`revision` +1），旧版本保留：历史日记的
// `styleSnapshotId` 仍指得到当时那一版，所以这里没有「删除风格」的入口，也不自动
// 重写历史日记。

import {
  SaveStyleParams,
  StyleIdParams,
  StyleProfile,
  StyleProfileInput,
} from '../core/Models';
import { CoreGateway, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';

/** 核心内置的默认风格 id：`styles.list` 里一定有它。 */
export const DEFAULT_STYLE_ID: string = 'sty_default';

/**
 * 把页面给的 `StyleProfileInput` 收敛成要发出去的入参。
 *
 * 这里沿用 SettingsStore 的三态纪律：
 * - 字段 `undefined` → **不写这个键**（核心用自己的默认值，等价于「不改」）；
 * - 字段 `null` → 明确清空这个偏好；
 * - 字段有值 → 设置。
 *
 * 所以不要用 `undefined` 表达清空，清空只能写 `null`。`revision` / `updatedAt` 不转发：
 * 它们由核心盖章，传了也不生效。`label` 去首尾空白（核心对空 label 报 `invalid_argument`，
 * 本 store 先在本地拦掉）。
 */
export function buildStyleInput(input: StyleProfileInput, label: string): StyleProfileInput {
  const profile: StyleProfileInput = {
    id: input.id.trim(),
    label: label,
  };
  if (input.lengthPreference !== undefined) {
    profile.lengthPreference = input.lengthPreference;
  }
  if (input.person !== undefined) {
    profile.person = input.person;
  }
  if (input.tone !== undefined) {
    profile.tone = input.tone;
  }
  if (input.emotionalTemperature !== undefined) {
    profile.emotionalTemperature = input.emotionalTemperature;
  }
  if (input.rhetoricLevel !== undefined) {
    profile.rhetoricLevel = input.rhetoricLevel;
  }
  if (input.examples !== undefined) {
    profile.examples = input.examples;
  }
  if (input.bannedExpressions !== undefined) {
    profile.bannedExpressions = input.bannedExpressions;
  }
  return profile;
}

/**
 * 风格状态容器：可被「风格设置」页面复用。
 *
 * 用法（页面侧）：
 * ```ts
 * const store = new StyleStore(dbPath);
 * await store.refresh();                       // styles.list（含 sty_default）
 * await store.getStyle(selectedId);            // styles.get；不存在就报 not_found，不回退
 * await store.saveStyle(edited, edited.revision); // 带锁保存；冲突会重读
 * this.styles = store.styles;
 * ```
 *
 * `loading`（读）与 `saving`（写）互斥：任一为 true 时新调用直接返回 `false` / `null`，
 * 不排队、不重放。
 */
export class StyleStore {
  /** 全部风格的**当前版本**（`styles.list` 原样，按核心排序；默认风格也在里面）。 */
  styles: StyleProfile[] = [];
  /** 最近一次 `styles.get` / `styles.save` 读到的单条风格；`null` = 没读过或不存在。 */
  current: StyleProfile | null = null;
  /** 正在读（list / get）。 */
  loading: boolean = false;
  /** 正在保存。 */
  saving: boolean = false;
  /** 最近一次保存是否因为 `revision` 过期被拒（为 true 时 store 已经重读过最新版本）。 */
  conflicted: boolean = false;
  /** 读/写失败时核心或宿主的原话（成功时是空哨兵）。 */
  error: UiError = EMPTY_UI_ERROR;
  /** 最近一次操作的结果说明；没有提示时是空串。 */
  message: string = '';

  private dbPath: string = '';

  constructor(dbPath: string) {
    this.dbPath = dbPath;
  }

  /**
   * 重读全部风格（`styles.list`）。失败时保留上一次成功读到的列表，并把原因写进 `error`。
   *
   * 读成功后，`current`（如果正指着某条）会用列表里的最新版本刷新一次——界面不会拿着
   * 一份比列表旧的单条对象去算 `expected_revision`。
   */
  async refresh(): Promise<boolean> {
    if (this.loading || this.saving) {
      return false;
    }
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'styles.list', null);
    this.loading = false;
    if (!result.ok) {
      this.error = uiErrorOf(result);
      return false;
    }
    this.styles = result.data as StyleProfile[];
    this.syncCurrentFromList();
    return true;
  }

  /**
   * 读一条风格的当前版本（`styles.get`）。
   *
   * 不存在时核心报 `not_found`——本 store **不回退**到默认风格，`current` 置 null，
   * `message` 说明原因；返回 `null`。这是刻意的：回退会让界面以为用户设过偏好。
   */
  async getStyle(styleId: string): Promise<StyleProfile | null> {
    if (this.loading || this.saving) {
      return null;
    }
    const id: string = styleId.trim();
    if (id.length === 0) {
      this.message = '风格 id 不能为空。';
      return null;
    }
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const params: StyleIdParams = { style_id: id };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'styles.get', params);
    this.loading = false;
    if (!result.ok) {
      const error: UiError = uiErrorOf(result);
      this.error = error;
      if (error.code === 'not_found') {
        this.current = null;
        this.message = '这条风格不存在；没有回退到默认风格（默认风格也要从 styles.list 里读）。';
      } else {
        this.message = '风格偏好没有读取完成。';
      }
      return null;
    }
    const profile: StyleProfile = result.data as StyleProfile;
    this.current = profile;
    this.upsert(profile);
    return profile;
  }

  /**
   * 保存一条风格（`styles.save`）：**每次落一个新的不可变版本**（`revision` +1）。
   *
   * `expectedRevision` 的两种语义：
   * - 传 `undefined`（省略）= **明确接受覆盖当前版本**，请求里根本不带
   *   `expected_revision` 这个键（`JSON.stringify` 会整个省掉）；
   * - 传数字 = 乐观锁。不符时核心报 `revision_conflict`，本 store **重读最新版本**、
   *   置 `conflicted`，返回 `null`——不拿旧 revision 重试，也不静默覆盖。
   *
   * 成功时把核心返回的新版本收进 `styles` 与 `current`，返回它；`revision` / `updatedAt`
   * 都以核心返回的为准。
   */
  async saveStyle(input: StyleProfileInput, expectedRevision?: number): Promise<StyleProfile | null> {
    if (this.saving || this.loading) {
      return null;
    }
    const id: string = input.id.trim();
    const label: string = input.label.trim();
    if (id.length === 0) {
      this.message = '风格 id 不能为空；没有保存。';
      return null;
    }
    if (label.length === 0) {
      this.message = '风格名称不能为空；没有保存。';
      return null;
    }
    this.saving = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const profile: StyleProfileInput = buildStyleInput(input, label);
    const params: SaveStyleParams = { profile: profile };
    if (expectedRevision !== undefined) {
      params.expected_revision = expectedRevision;
    }
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'styles.save', params);
    if (result.ok) {
      const saved: StyleProfile = result.data as StyleProfile;
      this.saving = false;
      this.upsert(saved);
      this.current = saved;
      const checked: string = expectedRevision === undefined
        ? '这次是明确覆盖，没有做版本检查'
        : '版本检查通过';
      this.message = `已保存「${saved.label}」第 ${saved.revision} 版；${checked}。` +
        '历史日记不会自动重写（styleSnapshotId 仍指当时那一版）。';
      return saved;
    }
    const error: UiError = uiErrorOf(result);
    if (error.code === 'revision_conflict') {
      this.conflicted = true;
      // 重读最新版本；**不拿旧 revision 重试**。读成功就把新值摆到界面上。
      const rereadParams: StyleIdParams = { style_id: profile.id };
      const reread: CoreResult = await CoreGateway.call(this.dbPath, 'styles.get', rereadParams);
      this.saving = false;
      if (reread.ok) {
        const latest: StyleProfile = reread.data as StyleProfile;
        this.upsert(latest);
        this.current = latest;
        this.message = '这条风格已被改动，已重读最新版本；请核对后再保存（没有拿旧 revision 重试）。';
      } else {
        this.error = uiErrorOf(reread);
        this.message = '这条风格已被改动，重读最新版本也失败了；上面的错误说明了原因。';
      }
      return null;
    }
    this.saving = false;
    this.error = error;
    if (error.code === 'invalid_argument') {
      this.message = '风格内容不合法（例如名称或示例太长）；没有保存，原设置保持不变。';
    } else {
      this.message = '风格偏好没有保存，原设置保持不变。';
    }
    return null;
  }

  /** 按 id 在已加载的列表里找一条（不读库）。 */
  find(styleId: string): StyleProfile | null {
    for (let i = 0; i < this.styles.length; i++) {
      if (this.styles[i].id === styleId) {
        return this.styles[i];
      }
    }
    return null;
  }

  /** 默认风格 `sty_default`（没有偏好时的真实配置）；列表还没读到时是 `null`。 */
  defaultStyle(): StyleProfile | null {
    return this.find(DEFAULT_STYLE_ID);
  }

  // ---------------------------------------------------------------- 内部

  /** 把单条风格写进列表：有则替换、无则追加（顺序按核心的列表顺序，不重排）。 */
  private upsert(profile: StyleProfile): void {
    for (let i = 0; i < this.styles.length; i++) {
      if (this.styles[i].id === profile.id) {
        this.styles[i] = profile;
        return;
      }
    }
    this.styles.push(profile);
  }

  /** 列表刷新后，把 `current` 换成同一 id 的最新版本。 */
  private syncCurrentFromList(): void {
    const current: StyleProfile | null = this.current;
    if (current === null) {
      return;
    }
    const fresh: StyleProfile | null = this.find(current.id);
    if (fresh !== null) {
      this.current = fresh;
    }
  }
}