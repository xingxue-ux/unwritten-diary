import 'dart:convert';
import 'dart:io';

import 'package:archive/archive.dart';
import 'package:crypto/crypto.dart';
import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart'
    show BridgeDiaryApi, BridgeSession;
import 'package:test/test.dart';

/// 备份**路径专用入口**（`*AtPath`）的端到端测试。
///
/// 契约 `backups.*` 收的是 `PlatformHost` 的限域票据，而票据解析尚未实现；这些
/// 入口只收文件系统路径，且**不在** `_wiredMethods`/`capabilities` 里（见
/// `bridge_diary_api_test.dart` 的能力断言）。这里同时钉住：票据形态在进核心之前
/// 被拒、不创建以票据命名的文件；create/restore 是**同步完成 + 已完成任务记录**，
/// `jobId` 指向 `succeeded` 记录，不是排队任务；非空目标/坏包被拒时当前库零改动、
/// 不留下假任务，恢复成功也不覆盖当前库。
///
/// 运行前先构建宿主产物：
///   cargo build --release
///   dart test
void main() {
  late Directory workDir;
  late String libraryPath;
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

    workDir = Directory.systemTemp.createTempSync('diary_backup_api_test');
    libraryPath = '${workDir.path}/library.sqlite';
    api = BridgeDiaryApi(
      coreLibraryPath: path,
      libraryPath: libraryPath,
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

  test('createBackupAtPath 返回已完成 job，包可检查且不含密钥/凭据', () async {
    await api.open();
    final capture = await _newCommittedCapture(api, 'backup-create');
    await _importTextAsset(api, 'backup-asset', '备份里要有原件。');

    const secret = 'sk-dart-backup-must-not-contain-0123456789';
    final provider = await api.saveProvider(
      const Provider(
        id: 'prv_backup_api',
        label: '备份隐私测试',
        adapterId: 'openai_compatible',
        capabilities: {ProviderCapability.generateText},
        enabled: true,
        endpoint: 'http://127.0.0.1:11434/v1/',
        model: 'qwen-test',
      ),
    );
    final withCredential = await api.saveProviderCredential(
      providerId: provider.id,
      secret: secret,
    );
    expect(withCredential.credentialRef, isNotNull);

    final package = '${workDir.path}/api-create.diarybak';
    final ticket = await api.createBackupAtPath(package);
    expect(ticket.jobId, isNotEmpty);

    final job = await api.getJob(ticket.jobId);
    expect(job.kind, 'backup');
    expect(job.state, JobState.succeeded);
    expect(job.targetIds, [package], reason: '任务的 targetIds 是包路径');
    expect(File(package).existsSync(), isTrue);

    final inspection = await api.inspectBackupAtPath(package);
    expect(inspection.issues, isEmpty);
    expect(inspection.schemaVersion, greaterThan(0));
    expect(inspection.assetCount, greaterThanOrEqualTo(1));

    // 契约第 8 节：API 密钥/临时凭据不进备份。逐条目解压后找明文，比只看文件名强。
    final archive = ZipDecoder().decodeBytes(File(package).readAsBytesSync());
    for (final entry in archive) {
      if (!entry.isFile) {
        continue;
      }
      final content = entry.content as List<int>;
      expect(
        utf8.decode(content, allowMalformed: true),
        isNot(contains(secret)),
        reason: '备份包条目 ${entry.name} 里出现了密钥明文',
      );
    }
    expect(
      archive
          .map((entry) => entry.name)
          .any(
            (name) => name.contains('secret') || name.contains('credential'),
          ),
      isFalse,
      reason: '备份包不应携带密钥/凭据文件',
    );

    // 不完整备份不是一个可恢复的备份：明确拒绝，不产出一个空壳包。
    final noAssets = '${workDir.path}/no-assets.diarybak';
    await expectLater(
      api.createBackupAtPath(noAssets, includeAssets: false),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );
    expect(File(noAssets).existsSync(), isFalse);

    // 已存在的包绝不覆盖。
    await expectLater(
      api.createBackupAtPath(package),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );
    expect((await api.getCapture(capture.id)).id, capture.id);
  });

  test('restoreBackupAtPath 恢复到新库，job 指向已完成记录，现库内容不变', () async {
    await api.open();
    final capture = await _newCommittedCapture(api, 'backup-restore');
    final before = await api.snapshot();
    final textBefore = (await api.getCapture(capture.id)).draftText;

    final package = '${workDir.path}/api-restore.diarybak';
    final backupTicket = await api.createBackupAtPath(package);
    expect((await api.getJob(backupTicket.jobId)).state, JobState.succeeded);

    final restoreTicket = await api.restoreBackupAtPath(
      packagePath: package,
      targetDir: '${workDir.path}/restored-for-api',
    );
    final job = await api.getJob(restoreTicket.jobId);
    expect(job.kind, 'restore');
    expect(job.state, JobState.succeeded);
    expect(job.targetIds, hasLength(2));
    final restoredLibrary = job.targetIds.first;
    expect(restoredLibrary, endsWith('library.sqlite'));
    expect(File(restoredLibrary).existsSync(), isTrue);

    // 恢复出来的库能打开，记录与正文都在。
    final restored = await BridgeSession.open(libraryPath: restoredLibrary);
    try {
      final info = await restored.info();
      expect(info.captureCount, greaterThanOrEqualTo(1));
      final page = await restored.listCaptures(limit: 100);
      expect(
        page.captures.any((item) => item.id == capture.id),
        isTrue,
        reason: '恢复出来的库应包含备份时的记录',
      );
      expect(
        page.captures.firstWhere((item) => item.id == capture.id).draftText,
        textBefore,
      );
    } finally {
      restored.dispose();
    }

    // 当前库没有被切走/覆盖：libraryId、记录数与正文都不变。
    final after = await api.snapshot();
    expect(after.coreInfo.libraryId, before.coreInfo.libraryId);
    expect(after.captureCount, before.captureCount);
    expect((await api.getCapture(capture.id)).draftText, textBefore);
  });

  test('restoreBackupAtPath 拒绝非空目标/坏包/覆盖现库，不写任务也不动现库', () async {
    await api.open();
    await _newCommittedCapture(api, 'backup-refuse');
    final package = '${workDir.path}/api-refuse.diarybak';
    await api.createBackupAtPath(package);

    final libraryFile = File(libraryPath);
    final fingerprintBefore = sha256
        .convert(libraryFile.readAsBytesSync())
        .toString();
    final restoreJobsBefore = (await api.listJobs(
      pageSize: 200,
    )).jobs.where((job) => job.kind == 'restore').length;
    final occupied = Directory('${workDir.path}/occupied')..createSync();
    File('${occupied.path}/keep.txt').writeAsStringSync('别动我');
    // 坏包先造好，再取目录快照：后面的对比只看「恢复动作」留下了什么。
    final broken = '${workDir.path}/api-broken.diarybak';
    final brokenBytes = File(package).readAsBytesSync();
    brokenBytes[brokenBytes.length ~/ 2] ^= 0xff;
    File(broken).writeAsBytesSync(brokenBytes);
    final childrenBefore = Directory(
      workDir.path,
    ).listSync().map((entry) => entry.path).toList()..sort();
    await expectLater(
      api.restoreBackupAtPath(packagePath: package, targetDir: occupied.path),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidState,
        ),
      ),
    );
    expect(File('${occupied.path}/keep.txt').readAsStringSync(), '别动我');

    // 契约的 restoreAsNewLibrary = false 等于「覆盖现库」，核心没有这个能力，拒绝。
    await expectLater(
      api.restoreBackupAtPath(
        packagePath: package,
        targetDir: '${workDir.path}/refused-overwrite',
        options: const RestoreOptions(restoreAsNewLibrary: false),
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );

    // 坏包：哈希/结构不过，拒绝且不留半成品。
    await expectLater(
      api.restoreBackupAtPath(
        packagePath: broken,
        targetDir: '${workDir.path}/broken-target',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.corruptedBackup,
        ),
      ),
    );

    final restoreJobsAfter = (await api.listJobs(
      pageSize: 200,
    )).jobs.where((job) => job.kind == 'restore').length;
    expect(
      restoreJobsAfter,
      restoreJobsBefore,
      reason: '被拒绝的恢复不能留下 succeeded 的假任务',
    );
    expect(
      sha256.convert(libraryFile.readAsBytesSync()).toString(),
      fingerprintBefore,
      reason: '拒绝恢复不能改动当前资料库',
    );
    final childrenAfter = Directory(
      workDir.path,
    ).listSync().map((entry) => entry.path).toList()..sort();
    expect(childrenAfter, childrenBefore, reason: '拒绝恢复不能留下暂存/半成品目录');
  });

  test('路径专用入口拒绝票据形态，不把 ticket: 字符串当路径/文件名', () async {
    await api.open();
    const ticket = 'ticket:backup-target-0199';
    final jobsBefore = (await api.listJobs(pageSize: 200)).jobs.length;

    Future<void> expectRejected(Future<Object?> future) async {
      await expectLater(
        future,
        throwsA(
          isA<DiaryException>()
              .having(
                (error) => error.code,
                'code',
                DiaryErrorCode.invalidArgument,
              )
              .having((error) => error.message, 'message', contains('票据')),
        ),
      );
    }

    await expectRejected(api.createBackupAtPath(ticket));
    await expectRejected(api.inspectBackupAtPath(ticket));
    await expectRejected(
      api.restoreBackupAtPath(
        packagePath: ticket,
        targetDir: '${workDir.path}/ticket-restore-target',
      ),
    );

    // 目标目录参数同样只收路径：先造一个真包，再只让 targetDir 是票据。
    final package = '${workDir.path}/ticket-guard.diarybak';
    await api.createBackupAtPath(package);
    await expectRejected(
      api.restoreBackupAtPath(
        packagePath: package,
        targetDir: 'ticket:restore-target',
      ),
    );

    // 被拒绝的调用一次都没进核心：没有以票据命名的文件/目录，也没有多余任务。
    expect(File(ticket).existsSync(), isFalse);
    expect(File('${workDir.path}/$ticket').existsSync(), isFalse);
    expect(
      Directory('${workDir.path}/ticket-restore-target').existsSync(),
      isFalse,
    );
    expect(
      Directory(workDir.path)
          .listSync(recursive: true)
          .map((entry) => entry.path)
          .where((path) => path.contains('ticket:')),
      isEmpty,
      reason: '票据字符串不能变成文件/目录',
    );
    expect(
      (await api.listJobs(pageSize: 200)).jobs.length,
      jobsBefore + 1,
      reason: '只有显式创建的那个真包写任务，被拒绝的票据调用不留任务',
    );
  });
}

Future<Capture> _newCommittedCapture(BridgeDiaryApi api, String suffix) async {
  final draft = await api.createDraft(operationId: '$suffix-create');
  final saved = await api.saveDraft(
    id: draft.id,
    text: '$suffix 的正文。',
    expectedRevision: draft.revision,
    operationId: '$suffix-save',
  );
  await api.commit(
    id: draft.id,
    expectedRevision: saved.revision,
    operationId: '$suffix-commit',
  );
  return api.getCapture(draft.id);
}

Future<ImportStatus> _importTextAsset(
  BridgeDiaryApi api,
  String suffix,
  String text,
) async {
  final capture = await _newCommittedCapture(api, suffix);
  final ticket = await api.prepareImport(
    captureId: capture.id,
    displayName: '$suffix.txt',
    mimeHint: 'text/plain',
    operationId: '$suffix-prepare',
  );
  final bytes = utf8.encode(text);
  File(ticket.stagingTicket).writeAsBytesSync(bytes);
  return api.finishImport(
    importId: ticket.importId,
    ticket: ticket.stagingTicket,
    manifest: ImportManifest(
      copiedBytes: bytes.length,
      sha256: sha256.convert(bytes).toString(),
      detectedMime: 'text/plain',
      originalName: '$suffix.txt',
    ),
  );
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
