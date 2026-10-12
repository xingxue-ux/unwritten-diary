import 'package:diary_api/diary_api.dart';
import 'package:test/test.dart';

void main() {
  group('枚举 wire 值', () {
    test('每个枚举都能从 wire 值往返解析', () {
      for (final value in CaptureState.values) {
        expect(CaptureState.fromWire(value.wire), value);
      }
      for (final value in SourceKind.values) {
        expect(SourceKind.fromWire(value.wire), value);
      }
      for (final value in AssetStorageState.values) {
        expect(AssetStorageState.fromWire(value.wire), value);
      }
      for (final value in ImportState.values) {
        expect(ImportState.fromWire(value.wire), value);
      }
      for (final value in Coverage.values) {
        expect(Coverage.fromWire(value.wire), value);
      }
      for (final value in LocatorType.values) {
        expect(LocatorType.fromWire(value.wire), value);
      }
      for (final value in JobState.values) {
        expect(JobState.fromWire(value.wire), value);
      }
      for (final value in RecordingState.values) {
        expect(RecordingState.fromWire(value.wire), value);
      }
      for (final value in DomainEventType.values) {
        expect(DomainEventType.fromWire(value.wire), value);
      }
      for (final value in ProviderCapability.values) {
        expect(ProviderCapability.fromWire(value.wire), value);
      }
    });

    test('未知 wire 值会抛出明确错误', () {
      expect(() => CaptureState.fromWire('not_a_state'),
          throwsA(isA<UnknownWireValueError>()));
    });

    test('关键 wire 值与契约写法一致', () {
      expect(DiaryVersionOrigin.userEdit.wire, 'user_edit');
      expect(Coverage.metadataOnly.wire, 'metadata_only');
      expect(LocatorType.textRange.wire, 'text_range');
      expect(JobState.waitingConfiguration.wire, 'waiting_configuration');
      expect(DomainEventType.diaryCurrentChanged.wire, 'diary.currentChanged');
      expect(ProviderCapability.transcribeAudio.wire, 'transcribe_audio');
    });
  });

  group('错误码', () {
    test('契约第 7 节的错误码都在枚举里', () {
      const contractCodes = [
        'storage_full',
        'permission_denied',
        'revision_conflict',
        'idempotency_conflict',
        'not_found',
        'invalid_state',
        'invalid_argument',
        'asset_missing',
        'integrity_failed',
        'unsupported_format',
        'provider_not_configured',
        'authentication_failed',
        'network_unavailable',
        'rate_limited',
        'quota_exceeded',
        'provider_timeout',
        'model_unavailable',
        'plugin_denied',
        'plugin_budget_exceeded',
        'incompatible_backup',
        'corrupted_backup',
        'search_expired',
        'cursor_expired',
      ];
      final declared = DiaryErrorCode.values.map((e) => e.wire).toSet();
      for (final code in contractCodes) {
        expect(declared, contains(code));
      }
    });

    test('invalid_argument 的 wire 值与往返（契约第 7 节）', () {
      expect(DiaryErrorCode.invalidArgument.wire, 'invalid_argument');
      expect(
        DiaryErrorCode.fromWire('invalid_argument'),
        DiaryErrorCode.invalidArgument,
      );
    });

    test('异常携带可重试标记与字段错误', () {
      const error = DiaryException(
        code: DiaryErrorCode.revisionConflict,
        message: '版本已变化',
        fieldErrors: {'expectedRevision': '过期'},
      );
      expect(error.retryable, isFalse);
      expect(error.fieldErrors['expectedRevision'], '过期');
      expect(error.toString(), contains('revision_conflict'));
    });

    test('会话过期异常自动可重试', () {
      expect(const SearchExpiredException(message: '会话过期').retryable, isTrue);
      expect(const SearchExpiredException(message: '会话过期').code,
          DiaryErrorCode.searchExpired);
      expect(const CursorExpiredException(message: '游标过期').code,
          DiaryErrorCode.cursorExpired);
    });
  });

  group('值对象语义', () {
    final base = DateTime.utc(2026, 9, 27, 9);

    test('Capture 的 copyWith 与相等判断', () {
      final capture = Capture(
        id: 'cap-1',
        revision: 1,
        state: CaptureState.draft,
        occurredAt: base,
        createdAt: base,
        updatedAt: base,
        timeZone: 'Asia/Shanghai',
        utcOffsetMinutes: 480,
        dayKey: '2026-09-27',
        orderedSourceIds: const ['src-1'],
        draftText: '风很大',
      );
      final saved = capture.copyWith(
        revision: 2,
        state: CaptureState.committed,
        processingSummary: const ProcessingSummary(unprocessed: 1),
      );

      expect(saved.id, capture.id);
      expect(saved.revision, 2);
      expect(saved.state, CaptureState.committed);
      expect(saved.processingSummary.unprocessed, 1);
      expect(saved, isNot(capture));
      expect(saved, saved.copyWith());
      expect(saved.orderedSourceIds, capture.orderedSourceIds);
    });

    test('Asset 的 copyWith 保留不可变字段', () {
      final asset = Asset(
        id: 'asset-1',
        objectRef: 'objects/ab/cd',
        originalName: '截图.png',
        detectedMime: 'image/png',
        byteSize: 1024,
        sha256: 'deadbeef',
        storageState: AssetStorageState.importing,
        importOrigin: ImportOrigin.picker,
        createdAt: base,
      );
      final ready = asset.copyWith(storageState: AssetStorageState.ready);
      expect(ready.storageState, AssetStorageState.ready);
      expect(ready.id, asset.id);
      expect(ready.byteSize, 1024);
    });

    test('Job 的 progress 可以为空，清除后仍相等', () {
      final job = Job(
        id: 'job-1',
        kind: 'transcribe',
        state: JobState.running,
        createdAt: base,
        updatedAt: base,
        progress: const JobProgress(completed: 3, total: 10),
      );
      expect(job.progress!.fraction, closeTo(0.3, 1e-9));
      expect(job.copyWith(clearProgress: true).progress, isNull);
      expect(job.copyWith(), job);
    });

    test('DiaryDay 可以清空当前版本', () {
      final day = DiaryDay(
        dayKey: '2026-09-27',
        timeZone: 'Asia/Shanghai',
        updatedAt: base,
        currentVersionId: 'v1',
      );
      expect(day.copyWith(clearCurrentVersion: true).currentVersionId, isNull);
      expect(day.copyWith(candidateVersionIds: const ['v2']).currentVersionId, 'v1');
    });

    test('Settings 更新保留时区语义', () {
      final settings = Settings(
        diaryTimeZone: 'Asia/Shanghai',
        revision: 1,
        updatedAt: base,
      );
      final updated =
          settings.copyWith(autoOrganizeTime: '23:10', revision: 2);
      expect(updated.autoOrganizeTime, '23:10');
      expect(updated.diaryTimeZone, 'Asia/Shanghai');
      expect(updated.revision, 2);
    });

    test('集合与 map 字段参与相等判断', () {
      const hitA = SearchHit(
        hitId: 'h1',
        groupId: 'g1',
        sourceKind: SourceKind.text,
        matchedBy: {MatchedBy.keyword, MatchedBy.semantic},
        coverage: Coverage.complete,
      );
      const hitB = SearchHit(
        hitId: 'h1',
        groupId: 'g1',
        sourceKind: SourceKind.text,
        matchedBy: {MatchedBy.semantic, MatchedBy.keyword},
        coverage: Coverage.complete,
      );
      expect(hitA, hitB);
      expect(hitA.hashCode, hitB.hashCode);
    });

    test('locator 工厂保留版本与区间', () {
      const locator = SourceLocator.text(
        sourceRevisionId: 'rev-1',
        range: TextRange(start: 3, end: 9),
      );
      expect(locator.type, LocatorType.textRange);
      expect(locator.textRange, const TextRange(start: 3, end: 9));
      expect(locator.sourceRevisionId, 'rev-1');

      const media = SourceLocator.media(
        sourceRevisionId: 'rev-2',
        isVideo: false,
        fromMs: 1000,
        toMs: 4000,
      );
      expect(media.type, LocatorType.audio);
      expect(media.startMs, 1000);
    });
  });
}