import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import BuildProfile from 'BuildProfile';
import { selectFirstRunSession } from '../session/AppDirectory';

/** Offline product entry: no HTTP probes or download helpers are imported. */
export default class EntryAbility extends UIAbility {
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    if (BuildProfile.DEBUG && want.parameters !== undefined) {
      const session = want.parameters['qa_session'];
      if (typeof session === 'string' && session.length > 0) {
        selectFirstRunSession(session);
      }
    }
    hilog.info(0, 'DiaryCore', 'OFFLINE_PRODUCT network=false');
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Startup', (error) => {
      if (error.code) { hilog.error(0, 'DiaryCore', 'OFFLINE_START_FAILED'); }
    });
  }
}
