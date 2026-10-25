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
// 4. **`styles.save` 是整行写入，没有「省略 = 不改」**：请求里缺少的字段会被核心当成
//    空值写进去（`tone: null`、`examples: []`）。所以本 store 的部分更新靠**补齐基底**
//    实现：`buildStyleInput(current, patch, label)` 把没出现在 patch 里的字段从
//    「这条风格当前完整 profile」抄过来。拿不到基底（新建风格）时用核心默认，并在
//    `message` 里明说「这是新风格，未填的用默认」。
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
 * 把页面给的「部分改动」补齐成一条**完整**的 `styles.save` 入参。
 *
 * ⚠️ 这里**没有**「字段 `undefined` = 不改」的三态语义：`styles.save` 是**整行写入**
 * （`crates/diary_core/src/styles.rs` 每次 INSERT 全部列；C ABI 的
 * `StyleProfileParams` 里 `examples` / `banned_expressions` 是 `#[serde(default)]`、
 * `tone` / `person` 等是 `Option`，省略 = 空数组 / None）。也就是说，请求体里**省略某个
 * 字段等于把它清空**，不是「保持原值」。曾经照搬 SettingsStore 的注释在这里写「省略 =
 * 不改」，结果是「只改个名字」会把用户已保存的语气、示例、禁用表达静默清成 NULL / []。
 *
 * 所以部分更新必须靠**补齐基底**实现：`current` 是这条风格在当前库里的完整 profile，
 * 没在 `patch` 里出现的字段一律从 `current` 抄过来，请求体永远是完整的一行。
 *
 * - `patch.X !== undefined` → 用 `patch.X`（包括显式写 `null` = 清空这个偏好）；
 * - `patch.X === undefined` → 用 `current.X`；
 * - `current === null`（**新建风格**）→ 未填字段用核心默认：可选字段 `null`、数组 `[]`。
 *   这种情况下界面必须说明「这是新风格，没填的用默认」（[`StyleStore.defaultsApplied`]
 *   与 `message` 会写出来），不能让人以为是从某条已有风格继承来的。
 *
 * `revision` / `updatedAt` 不转发：它们由核心盖章，传了也不生效。`label` 去首尾空白
 * （核心对空 label 报 `invalid_argument`，本 store 先在本地拦掉）。
 */
export function buildStyleInput(
  current: StyleProfile | null,
  patch: StyleProfileInput,
  label: string,
): StyleProfileInput {
  const base = current;
  const profile: StyleProfileInput = {
    id: patch.id.trim(),
    label: label,
    lengthPreference: patch.lengthPreference !== undefined
      ? patch.lengthPreference
      : (base === null ? null : base.lengthPreference),
    person: patch.person !== undefined
      ? patch.person
      : (base === null ? null : base.person),
    tone: patch.tone !== undefined
      ? patch.tone
      : (base === null ? null : base.tone),
    emotionalTemperature: patch.emotionalTemperature !== undefined
      ? patch.emotionalTemperature
      : (base === null ? null : base.emotionalTemperature),
    rhetoricLevel: patch.rhetoricLevel !== undefined
      ? patch.rhetoricLevel
      : (base === null ? null : base.rhetoricLevel),
    examples: patch.examples !== undefined
      ? patch.examples
      : (base === null ? [] : base.examples),
    bannedExpressions: patch.bannedExpressions !== undefined
      ? patch.bannedExpressions
      : (base === null ? [] : base.bannedExpressions),
  };
  return profile;
}

/** `patch` 是不是已经把 8 个可选字段全给齐了（给齐就不需要再读基底）。 */
function isCompleteStyleInput(input: StyleProfileInput): boolean {
  return input.lengthPreference !== undefined && input.person !== undefined &&
    input.tone !== undefined && input.emotionalTemperature !== undefined &&
    input.rhetoricLevel !== undefined && input.examples !== undefined &&
    input.bannedExpressions !== undefined;
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
  /**
   * 最近一次保存是不是「新建风格 + 未给全字段」，因而已填字段用了核心默认值。
   *
   * 只在 `saveStyle` 里复位/置位：`true` 时 `message` 会写明「这是新风格，未填的用默认」。
   * 页面可以据此在表单上提示「新风格，空字段不会继承任何已有偏好」。
   */
  defaultsApplied: boolean = false;
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
    this.defaultsApplied = false;
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
      this.message = '请先选择风格。';
      return null;
    }
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    this.defaultsApplied = false;
    const params: StyleIdParams = { style_id: id };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'styles.get', params);
    this.loading = false;
    if (!result.ok) {
      const error: UiError = uiErrorOf(result);
      this.error = error;
      if (error.code === 'not_found') {
        this.current = null;
        this.message = '这条风格已不存在，请重新选择。';
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
      this.message = '请先选择风格。';
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
    this.defaultsApplied = false;

    // ---- 部分更新必须先拿到基底（styles.save 是整行写入，省略字段 = 清空） ----
    // 只认**刚读过/刚保存过**的那条（`current`），不拿 `styles` 列表里的旧快照当基底：
    // 列表可能已经过期，拿旧值补齐会把别人刚写进去的语气/示例又覆盖回去。
    // 没有可用基底时用 `styles.get` 读一次当前完整 profile；读失败（不是 not_found）
    // 就**不保存**：宁可报错，也不能把用户已保存的语气/示例清空。`not_found` = 这里
    // 确实还没有这条风格 → 新建，用核心默认。
    const partial: boolean = !isCompleteStyleInput(input);
    let base: StyleProfile | null =
      (this.current !== null && this.current.id === id) ? this.current : null;
    if (partial && base === null) {
      const probeParams: StyleIdParams = { style_id: id };
      const probe: CoreResult = await CoreGateway.call(this.dbPath, 'styles.get', probeParams);
      if (probe.ok) {
        base = probe.data as StyleProfile;
        this.upsert(base);
      } else {
        const probeError: UiError = uiErrorOf(probe);
        if (probeError.code !== 'not_found') {
          this.saving = false;
          this.error = probeError;
          this.message = '这条风格的当前内容没有读出来，无法判断「哪些字段要保留」；' +
            '为了不清空已保存的语气/示例，这次没有保存。';
          return null;
        }
      }
    }
    const defaultsApplied: boolean = base === null;
    this.defaultsApplied = defaultsApplied;
    const profile: StyleProfileInput = buildStyleInput(base, input, label);
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
      this.message = '风格已保存，仅影响今后整理的日记。';
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
        this.message = '风格已更新，请核对后再保存。';
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