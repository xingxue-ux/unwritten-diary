// 麦克风权限：**声明在 module.json5，运行时再向用户要**。
//
// 两条硬口径：
// 1. 被拒就如实返回 `granted=false`，调用方必须提示用户（不静默、不假装在录）；
// 2. `authResults` 原样带出去（0=授权，-1=拒绝，2=声明/条件不对），日志里能对照。

import { abilityAccessCtrl, bundleManager, common, Permissions } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryRec';
const MARK: string = 'DIARY_REC';
const MICROPHONE: Permissions = 'ohos.permission.MICROPHONE';

/** 权限结果：`checked=true` 表示走到过「查当前状态」；`auth_results` 是系统原样返回。 */
export class PermissionOutcome {
  granted: boolean = false;
  checked: boolean = false;
  already_granted: boolean = false;
  auth_results: number[] = [];
  message: string = '';
}

/** 查 + 要麦克风权限。已经授权时不会再弹框。 */
export async function ensureMicrophonePermission(
  context: common.UIAbilityContext,
): Promise<PermissionOutcome> {
  const outcome = new PermissionOutcome();
  const atManager = abilityAccessCtrl.createAtManager();

  try {
    const bundleInfo = bundleManager.getBundleInfoForSelfSync(
      bundleManager.BundleFlag.GET_BUNDLE_INFO_WITH_APPLICATION,
    );
    const status = atManager.checkAccessTokenSync(
      bundleInfo.appInfo.accessTokenId,
      MICROPHONE,
    );
    outcome.checked = true;
    if (status === abilityAccessCtrl.GrantStatus.PERMISSION_GRANTED) {
      outcome.granted = true;
      outcome.already_granted = true;
      outcome.message = '已经授权';
      hilog.info(DOMAIN, TAG, '%{public}s PERM alreadyGranted permission=%{public}s',
        MARK, MICROPHONE);
      return outcome;
    }
    hilog.info(DOMAIN, TAG, '%{public}s PERM state=denied permission=%{public}s',
      MARK, MICROPHONE);
  } catch (e) {
    const be = e as BusinessError;
    outcome.message = '暂时无法检查权限，请重试。';
    hilog.warn(DOMAIN, TAG, '%{public}s PERM checkFailed code=%{public}d message=%{public}s',
      MARK, be.code, be.message);
  }

  try {
    const result = await atManager.requestPermissionsFromUser(context, [MICROPHONE]);
    outcome.auth_results = result.authResults;
    outcome.granted = result.authResults.length > 0 && result.authResults[0] === 0;
    outcome.message = outcome.granted ? '用户已授权' : '用户拒绝了麦克风权限';
    let results = '';
    for (let i = 0; i < result.authResults.length; i++) {
      results += `${result.authResults[i]},`;
    }
    hilog.info(DOMAIN, TAG, '%{public}s PERM request granted=%{public}s authResults=[%{public}s] permissions=%{public}s',
      MARK, outcome.granted ? 'true' : 'false', results, result.permissions.join(','));
    return outcome;
  } catch (e) {
    const be = e as BusinessError;
    outcome.granted = false;
    outcome.message = '未能获得麦克风权限，请在系统设置中授权。';
    hilog.error(DOMAIN, TAG, '%{public}s PERM requestFailed code=%{public}d message=%{public}s',
      MARK, be.code, be.message);
    return outcome;
  }
}
