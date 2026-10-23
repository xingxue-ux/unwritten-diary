import { common } from '@kit.AbilityKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import BuildProfile from 'BuildProfile';

let firstRunSession: string = '';

/** Debug-only first-use UI acceptance; normal launches always use the real library. */
export function selectFirstRunSession(session: string): void {
  if (BuildProfile.DEBUG && /^[a-z0-9-]{1,60}$/.test(session)) {
    firstRunSession = session;
  } else {
    firstRunSession = '';
  }
}

export function appFilesDir(context: common.UIAbilityContext): string {
  if (BuildProfile.DEBUG && /^[a-z0-9-]{1,60}$/.test(firstRunSession)) {
    return `${context.filesDir}/first-run-ui/${firstRunSession}`;
  }
  return context.filesDir;
}

/** Only an explicitly selected debug session can be removed; never the real library. */
export function clearFirstRunSession(context: common.UIAbilityContext): void {
  const root: string = appFilesDir(context);
  if (root === context.filesDir || !root.startsWith(`${context.filesDir}/first-run-ui/`)) {
    throw new Error('未选择隔离测试目录');
  }
  if (fs.accessSync(root)) { fs.rmdirSync(root); }
}
