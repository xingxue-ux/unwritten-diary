import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// B7 的桥接端到端：Dart → `createBackup` / `inspectBackup` / `restoreBackup`
/// → 真核心 + 真 SQLite + 真 zip 包。
///
/// 覆盖三件只有从 Dart 侧才看得出来的事：
/// 1. **能力项**：桥接把 `backups.create` / `backups.inspect` / `backups.restore`
///    如实报出来（Dart 适配层另有一层窄化，那是 `BridgeDiaryApi` 的事）；
/// 2. **快照边界与原件**：备份里有原件、`inspect` 逐条目校验通过，恢复出来的库能
///    打开、记录还在、原件数对得上；
/// 3. **E35 口径**：损坏的包被拒（`corrupted_backup`）、目标目录不留东西、
///    当前资料库的库文件哈希前后一致。
///
/// 运行前先构建宿主产物：
///   cargo build --release            （仓库根目录）
///   dart test                        （本包目录）
void main() {
  late Directory root;

  setUpAll(() async {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(
      File(path).existsSync(),
      isTrue,
      reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release',
    );
    await RustLib.init(externalLibrary: ExternalLibrary.open(path));
    root = Directory.systemTemp.createTempSync('diary_backup_test');
  });

  tearDownAll(() {
    try {
      root.deleteSync(recursive: true);
    } on FileSystemException {
      // 删不掉不影响结论。
    }
  });

  /// 一条记录 + 一个原件，返回 (记录 id, 原件字节)。
  Future<(String, List<int>)> seed(BridgeSession session) async {
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'b-create',
    );
    final saved = await session.saveDraft(
      captureId: draft.id,
      text: '桥接备份测试的正文',
      expectedRevision: draft.revision,
      operationId: 'b-save',
    );
    await session.commit(
      captureId: draft.id,
      expectedRevision: saved.revision,
      operationId: 'b-commit',
    );

    final bytes = utf8.encode('原件里的字节也要一起备份');
    final ticket = await session.prepareImport(
      captureId: draft.id,
      displayName: '原件.txt',
      mimeHint: 'text/plain',
      origin: ImportOrigin.picker,
      operationId: 'b-import',
    );
    File(ticket.stagingTicket).writeAsBytesSync(bytes);
    final imported = await session.finishImport(
      importId: ticket.importId,
      stagingTicket: ticket.stagingTicket,
      manifest: ImportManifest(
        copiedBytes: bytes.length,
        sha256: sha256.convert(bytes).toString(),
        detectedMime: 'text/plain',
        originalName: '原件.txt',
      ),
    );
    expect(imported.state, ImportState.ready);
    return (draft.id, bytes);
  }

  test('备份 → 检查 → 恢复到新库，损坏包被拒且现库零改动', () async {
    final caseDir = Directory.systemTemp.createTempSync('diary_backup_case');
    final libDir = Directory('${caseDir.path}/lib')..createSync();
    final session = await BridgeSession.open(
      libraryPath: '${libDir.path}/library.sqlite',
    );
    try {
      final (captureId, originalBytes) = await seed(session);

      // 能力项：桥接真的把这三个方法接上了。
      final info = await session.info();
      expect(info.apiVersion, '1.0');
      expect(
        info.capabilities,
        containsAll(['backups.create', 'backups.inspect', 'backups.restore']),
      );

      // 备份。
      final package = '${caseDir.path}/bridge.diarybak';
      final report = await session.createBackup(targetPath: package);
      expect(report.packagePath, package);
      expect(File(package).existsSync(), isTrue);
      expect(report.blobCount, 1);
      expect(report.counts.captures, 1);
      expect(report.counts.blobs, 1);
      expect(report.apiVersion, info.apiVersion);

      // 检查：逐条目核过哈希，没有问题。
      final inspection = await session.inspectBackup(packagePath: package);
      expect(inspection.issues, isEmpty);
      expect(inspection.hashesVerified, isTrue);
      expect(inspection.compatible, isTrue);
      expect(inspection.blobCount, 1);

      // 现库指纹：拒绝类操作前后要比。
      final libraryFile = File('${libDir.path}/library.sqlite');
      final fingerprintBefore = sha256
          .convert(libraryFile.readAsBytesSync())
          .toString();

      // E35 的坏包先造好，再取目录快照——这样后面的目录对比只看恢复动作留下了什么。
      final broken = '${caseDir.path}/broken.diarybak';
      final brokenBytes = File(package).readAsBytesSync();
      brokenBytes[brokenBytes.length ~/ 2] ^= 0xff;
      File(broken).writeAsBytesSync(brokenBytes);
      final childrenBefore = caseDir.listSync().map((e) => e.path).toList()
        ..sort();

      final brokenInspection = await session.inspectBackup(packagePath: broken);
      expect(brokenInspection.issues, isNotEmpty);
      expect(brokenInspection.hashesVerified, isFalse);

      final brokenTarget = '${caseDir.path}/restored-broken';
      await expectLater(
        session.restoreBackup(packagePath: broken, targetDir: brokenTarget),
        throwsA(
          isA<BridgeError>().having(
            (error) => error.code,
            'code',
            'corrupted_backup',
          ),
        ),
      );
      expect(Directory(brokenTarget).existsSync(), isFalse);
      expect(
        caseDir.listSync().map((e) => e.path).toList()..sort(),
        childrenBefore,
        reason: '恢复失败不该在目标旁边留下半成品',
      );
      expect(
        sha256.convert(libraryFile.readAsBytesSync()).toString(),
        fingerprintBefore,
        reason: '拒绝恢复不该改动当前资料库',
      );

      // E35：非空目标目录 → invalid_state。
      final occupied = Directory('${caseDir.path}/occupied')..createSync();
      File('${occupied.path}/别人东西.txt').writeAsStringSync('别动我');
      await expectLater(
        session.restoreBackup(packagePath: package, targetDir: occupied.path),
        throwsA(
          isA<BridgeError>().having(
            (error) => error.code,
            'code',
            'invalid_state',
          ),
        ),
      );
      expect(File('${occupied.path}/别人东西.txt').existsSync(), isTrue);
      expect(
        sha256.convert(libraryFile.readAsBytesSync()).toString(),
        fingerprintBefore,
      );

      // 正常路径：恢复到新目录。
      final target = '${caseDir.path}/restored';
      final restore = await session.restoreBackup(
        packagePath: package,
        targetDir: target,
      );
      expect(File(restore.libraryPath).existsSync(), isTrue);
      expect(restore.counts.captures, 1);
      expect(restore.blobCount, 1);

      // 恢复出来的原件按内容寻址路径落盘，哈希与原件一致。
      final restoredBytes = File(
        '${restore.rootDir}/blobs/${sha256.convert(originalBytes).toString().substring(0, 2)}'
        '/${sha256.convert(originalBytes)}',
      ).readAsBytesSync();
      expect(restoredBytes, originalBytes);

      // 恢复出来的库能打开、记录还在；当前资料库也能继续读。
      final restored = await BridgeSession.open(
        libraryPath: restore.libraryPath,
      );
      try {
        final snapshot = await restored.info();
        expect(snapshot.captureCount, 1);
        final page = await restored.listCaptures(limit: 10);
        expect(page.captures.single.id, captureId);
        expect(page.captures.single.draftText, '桥接备份测试的正文');
      } finally {
        restored.dispose();
      }
      expect((await session.info()).captureCount, 1);
    } finally {
      session.dispose();
      try {
        caseDir.deleteSync(recursive: true);
      } on FileSystemException {
        // 删不掉不影响结论。
      }
    }
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
  throw UnsupportedError('未支持的平台：${Platform.operatingSystem}');
}
