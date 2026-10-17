import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:test/test.dart';

/// B4c 的正文编辑 / 指定当前版本 / 合并：**只经契约接口**（`DiaryApi`）走真核心 + 真 SQLite。
///
/// 「新版本成为当前、旧版退出默认视图、修订链、冲突零改动、幂等、来源可追溯」这些
/// **语义**在核心侧由 `crates/diary_core/tests/diary_versions_test.rs`（14 条）钉住；
/// 这里钉的是**适配层**：三个方法真的接上了（能力清单如实列出）、参数如实传下去、
/// 核心的错误码翻成契约的枚举，而且失败时一个版本都不产生。
///
/// 这一条不联网、不配置任何远端服务，也不造日记版本——造一版要一次真实模型调用，
/// 而 CI 不联网。失败路径 + 能力声明就是适配层能独立验证的部分（与 B4b 的
/// `bridge_diary_generation_test.dart` 同一分工）。
///
/// 运行前先构建宿主产物：
///   cargo build --release            （仓库根目录）
///   dart test                        （本包目录）
void main() {
  late Directory workDir;
  late BridgeDiaryApi api;

  setUpAll(() async {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(
      File(path).existsSync(),
      isTrue,
      reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release',
    );
    workDir = Directory.systemTemp.createTempSync('diary_diary_versions_test');
    api = BridgeDiaryApi(
      coreLibraryPath: path,
      libraryPath: '${workDir.path}/library.sqlite',
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
    await api.open();

    // 这一天的记录：`getDay` 才有东西可读（空日报 `not_found`）。
    final draft = await api.createDraft(
      occurredAt: DateTime.utc(2026, 10, 16, 2),
      operationId: 'op-v-create',
    );
    await api.saveDraft(
      id: draft.id,
      text: '今天去公园散步。',
      expectedRevision: draft.revision,
      operationId: 'op-v-save',
    );
    await api.commit(
      id: draft.id,
      expectedRevision: draft.revision + 1,
      operationId: 'op-v-commit',
    );
  });

  tearDownAll(() async {
    await api.close();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删除失败不该让测试失败；临时目录留给系统清理。
    }
  });

  test('能力清单如实列出 saveEdit / setCurrent / merge', () async {
    final snapshot = await api.snapshot();
    expect(snapshot.coreInfo.capabilities, contains('diary.saveEdit'));
    expect(snapshot.coreInfo.capabilities, contains('diary.setCurrent'));
    expect(snapshot.coreInfo.capabilities, contains('diary.merge'));

    final day = await api.getDay('2026-10-16');
    expect(day.currentVersionId, isNull, reason: '还没有任何版本');
    expect(day.candidateVersionIds, isEmpty);
    // #101：这一天的 revision 是持久单调计数，桥接如实带过来；没有推进过的天是 0。
    expect(day.revision, 0, reason: '没有任何版本与指针变更就没有推进');
  });

  test('版本不存在时三个方法都报契约错误码，且不产生任何版本', () async {
    // 编辑：基版不存在 → notFound。
    await expectLater(
      api.saveDiaryEdit(
        baseVersionId: 'dver_不存在',
        body: '改过的正文。',
        expectedRevision: 1,
        operationId: 'op-v-edit-missing',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.notFound,
        ),
      ),
    );

    // 指定当前：版本不存在 → notFound；dayKey 形状不对 → invalidArgument。
    await expectLater(
      api.setCurrentVersion(
        dayKey: '2026-10-16',
        versionId: 'dver_不存在',
        expectedRevision: 1,
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.notFound,
        ),
      ),
    );
    await expectLater(
      api.setCurrentVersion(
        dayKey: '16-10-2026',
        versionId: 'dver_不存在',
        expectedRevision: 1,
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );

    // 合并：基版不存在 → notFound。
    await expectLater(
      api.mergeDiary(
        baseVersionId: 'dver_不存在',
        candidateVersionIds: const ['dver_另一个'],
        body: '合并后的正文。',
        expectedRevision: 1,
        operationId: 'op-v-merge-missing',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.notFound,
        ),
      ),
    );

    // 入参本身不合法时不碰库：候选列表为空 / 空正文 → invalidArgument。
    await expectLater(
      api.mergeDiary(
        baseVersionId: 'dver_不存在',
        candidateVersionIds: const [],
        body: '合并后的正文。',
        expectedRevision: 1,
        operationId: 'op-v-merge-empty',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );
    await expectLater(
      api.saveDiaryEdit(
        baseVersionId: 'dver_不存在',
        body: '   ',
        expectedRevision: 1,
        operationId: 'op-v-edit-empty',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );

    // 失败就是零改动：这一天仍然一个版本都没有，当前版本仍然为空。
    final page = await api.listVersions('2026-10-16');
    expect(page.versions, isEmpty);
    final day = await api.getDay('2026-10-16');
    expect(day.currentVersionId, isNull);
    expect(day.candidateVersionIds, isEmpty);
    expect(day.revision, 0, reason: '被拒绝的调用不推进 revision');
  });
}

String _defaultLibraryPath() {
  if (Platform.isLinux) {
    return '../../target/release/libdiary_bridge.so';
  }
  if (Platform.isMacOS) {
    return '../../target/release/libdiary_bridge.dylib';
  }
  if (Platform.isWindows) {
    return '../../target/release/diary_bridge.dll';
  }
  throw UnsupportedError('未知平台：${Platform.operatingSystem}');
}