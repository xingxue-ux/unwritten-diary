import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:test/test.dart';

/// B4b 的日记生成与版本：**只经契约接口**（`DiaryApi`）走真核心 + 真 SQLite。
///
/// 这条测试不联网也不需要配置任何远端服务。任务的等待/失败状态属于 `jobs.*`
/// （适配层还没接那一路），由 `bridge_diary_job_test.dart` 用原始会话钉住；
/// 这里钉的是适配层的映射与「没有服务时不产出假版本」。
///
/// 运行前先构建宿主产物：
///   cargo build --release            （仓库根目录）
///   dart test                        （本包目录）
void main() {
  late Directory workDir;
  late BridgeDiaryApi api;

  setUpAll(() {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(
      File(path).existsSync(),
      isTrue,
      reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release',
    );
    workDir = Directory.systemTemp.createTempSync('diary_diary_adapter_test');
    api = BridgeDiaryApi(
      coreLibraryPath: path,
      libraryPath: '${workDir.path}/library.sqlite',
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
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

  test('生成票据先冻结输入，没有服务时停在 waiting_configuration', () async {
    final opened = await api.open();
    expect(opened.coreInfo.capabilities, contains('diary.generate'));

    // 这一天的记录（记录文字本身照常可搜）。
    final draft = await api.createDraft(
      occurredAt: DateTime.utc(2026, 10, 16, 2),
      operationId: 'op-d-create',
    );
    await api.saveDraft(
      id: draft.id,
      text: '今天去公园散步，风有点大。',
      expectedRevision: draft.revision,
      operationId: 'op-d-save',
    );
    await api.commit(
      id: draft.id,
      expectedRevision: draft.revision + 1,
      operationId: 'op-d-commit',
    );

    final day = await api.getDay('2026-10-16');
    expect(day.dayKey, '2026-10-16');
    expect(day.materialCounts.captures, 1);
    expect(day.materialCounts.processed, 1);
    expect(day.materialCounts.pending, 0);
    expect(day.coverage, Coverage.complete);
    expect(day.currentVersionId, isNull, reason: '还没有任何版本');
    expect(day.candidateVersionIds, isEmpty);

    // 生成：返回票据（输入已冻结），但一个服务都没配 → 任务等配置。
    final ticket = await api.generateDiary(
      dayKey: '2026-10-16',
      reason: DiaryGenerationReason.manual,
      operationId: 'op-d-gen',
    );
    expect(ticket.jobId, isNotEmpty);
    expect(ticket.sourceSnapshotId, isNotEmpty);

    // 任务停在「等配置」这件事由 `jobs.get` 看（适配层还没有接 jobs.*，
    // 那一路在 `bridge_diary_job_test.dart` 用原始会话钉住）。
    // 这里断言的是适配层能看到的：没有版本、更没有空正文冒充成功；检索照常可用。
    final page = await api.listVersions('2026-10-16');
    expect(page.versions, isEmpty);
    expect(page.nextCursor, isNull);
    final hits = await api.startSearch(
      request: const SearchRequest(
        query: '公园',
        mode: SearchMode.keyword,
      ),
      queryRevision: 1,
    );
    expect(hits.results, isNotEmpty, reason: '用户自己写的文字照常搜得到');

    // 幂等：同一条 operationId 重试返回原票据。
    final again = await api.generateDiary(
      dayKey: '2026-10-16',
      reason: DiaryGenerationReason.manual,
      operationId: 'op-d-gen',
    );
    expect(again.jobId, ticket.jobId);
    expect(again.sourceSnapshotId, ticket.sourceSnapshotId);

    // 读一条不存在的版本：契约的错误码，不是崩溃。
    await expectLater(
      api.getVersion('dver_不存在'),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.notFound,
        ),
      ),
    );
  });

  test('空日与无材料日：not_found / invalid_argument，都不排空任务', () async {
    await expectLater(
      api.getDay('2020-01-01'),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.notFound,
        ),
      ),
    );
    await expectLater(
      api.generateDiary(
        dayKey: '2020-01-01',
        reason: DiaryGenerationReason.manual,
        operationId: 'op-d-empty',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );
    // 点名一个还不存在的风格：not_found（styles.* 还没实现）。
    await expectLater(
      api.generateDiary(
        dayKey: '2026-10-16',
        styleId: 'sty_x',
        reason: DiaryGenerationReason.manual,
        operationId: 'op-d-style',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.notFound,
        ),
      ),
    );
  });
}

String _defaultLibraryPath() {
  if (Platform.isLinux) {
    return '../../target/release/libdiary_bridge.so';
  }
  if (Platform.isWindows) {
    return r'..\..\target\x86_64-pc-windows-msvc\release\diary_bridge.dll';
  }
  if (Platform.isMacOS) {
    return '../../target/release/libdiary_bridge.dylib';
  }
  throw UnsupportedError('未知平台：${Platform.operatingSystem}');
}