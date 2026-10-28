// 票据解析协议（issue #170 方案 A）的契约面测试。
//
// 这条协议只写一次边界：「宿主把票据解析成绝对路径，核心只认路径」。
// 桌面端的票据本来就是路径，所以默认实现是恒等映射；需要限域的平台覆盖它。
import 'package:diary_api/diary_api.dart';
import 'package:test/test.dart';

/// 只覆盖 `resolveTicket` 的宿主；其余抽象成员由 `noSuchMethod` 兜住
/// （本测试不碰系统能力，碰了就该抛 NoSuchMethodError）。
class _ResolvingHost extends PlatformHost {
  _ResolvingHost(this.tickets);

  final Map<String, String> tickets;
  final List<(String, TicketPurpose)> calls = [];

  @override
  Future<String> resolveTicket(
    String ticket, {
    required TicketPurpose purpose,
  }) async {
    calls.add((ticket, purpose));
    final resolved = tickets[ticket];
    if (resolved == null) {
      throw ArgumentError.value(ticket, 'ticket', '未知、过期或超出授权范围的票据');
    }
    return resolved;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

/// 不覆盖任何方法的宿主：验默认实现（桌面端恒等映射）。
class _DefaultHost extends PlatformHost {
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  group('PlatformHost.resolveTicket', () {
    test('默认实现是恒等映射：桌面端的票据就是真实路径', () async {
      final host = _DefaultHost();
      final resolved = await host.resolveTicket(
        r'C:\Users\me\Documents\backup.zip',
        purpose: TicketPurpose.backupPackage,
      );
      expect(resolved, r'C:\Users\me\Documents\backup.zip');
    });

    test('覆盖后按用途解析，且用途被如实传给宿主', () async {
      final host = _ResolvingHost({
        'ticket:backup-target': r'C:\sandbox\backups\2026-10-21.zip',
        'ticket:package': r'C:\sandbox\incoming\old.zip',
      });

      final target = await host.resolveTicket(
        'ticket:backup-target',
        purpose: TicketPurpose.backupTarget,
      );
      final package = await host.resolveTicket(
        'ticket:package',
        purpose: TicketPurpose.backupPackage,
      );

      expect(target, r'C:\sandbox\backups\2026-10-21.zip');
      expect(package, r'C:\sandbox\incoming\old.zip');
      // 用途必须区分：一张票据不该被跨用途复用。
      expect(host.calls, [
        ('ticket:backup-target', TicketPurpose.backupTarget),
        ('ticket:package', TicketPurpose.backupPackage),
      ]);
    });

    test('未知票据抛 ArgumentError，不静默当成路径', () async {
      final host = _ResolvingHost({});
      expect(
        () => host.resolveTicket('ticket:gone', purpose: TicketPurpose.backupPackage),
        throwsA(isA<ArgumentError>()),
      );
    });

    test('四种用途都在枚举里，契约里的票据点一个不漏', () {
      expect(TicketPurpose.values, hasLength(4));
      expect(
        TicketPurpose.values.map((p) => p.name).toSet(),
        {
          'backupTarget',
          'backupPackage',
          'recordingStaging',
          'videoAudioTarget',
        },
      );
    });
  });
}