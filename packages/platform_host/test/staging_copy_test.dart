import 'dart:io';
import 'dart:async';
import 'package:platform_host/platform_host.dart';
import 'package:test/test.dart';

void main() {
  late Directory directory;
  late File target;
  setUp(() async {
    directory = await Directory.systemTemp.createTemp('diary-copy-');
    target = File('${directory.path}/ticket');
  });
  tearDown(() => directory.delete(recursive: true));
  test(
    'cancels while the source is stalled without waiting for more bytes',
    () async {
      final source = StreamController<List<int>>();
      final cancellation = CopyCancellation();
      final started = Completer<void>();
      final result = copyToStaging(
        source: source.stream,
        destination: target,
        cancellation: cancellation,
        onProgress: (_) => started.complete(),
      );
      source.add([1]);
      await started.future;
      final assertion = expectLater(result, throwsA(isA<CopyCancelled>()));
      cancellation.cancel();
      await assertion.timeout(const Duration(seconds: 2));
      expect(source.hasListener, isFalse);
      await source.close();
    },
  );
  test('hashes and flushes multiple chunks without changing bytes', () async {
    final progress = <int>[];
    final result = await copyToStaging(
      source: Stream.fromIterable([
        [97],
        [98, 99],
      ]),
      destination: target,
      cancellation: CopyCancellation(),
      onProgress: progress.add,
    );
    expect(result.byteSize, 3);
    expect(
      result.sha256,
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
    expect(await target.readAsBytes(), [97, 98, 99]);
    expect(progress, [1, 3]);
  });
  test(
    'cancellation preserves partial staging and never returns success',
    () async {
      final cancellation = CopyCancellation();
      await expectLater(
        copyToStaging(
          source: Stream.fromIterable([
            [1],
            [2],
          ]),
          destination: target,
          cancellation: cancellation,
          onProgress: (_) => cancellation.cancel(),
        ),
        throwsA(isA<CopyCancelled>()),
      );
      expect(await target.readAsBytes(), [1]);
    },
  );
  test('rejects over-limit chunk before writing it', () async {
    await expectLater(
      copyToStaging(
        source: Stream.fromIterable([
          [1],
          [2, 3],
        ]),
        destination: target,
        cancellation: CopyCancellation(),
        maxBytes: 2,
      ),
      throwsA(isA<CopyTooLarge>()),
    );
    expect(await target.readAsBytes(), [1]);
  });
}
