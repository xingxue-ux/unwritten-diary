import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:flutter_timezone/flutter_timezone.dart';
import 'package:path/path.dart' as path;
import 'package:path_provider/path_provider.dart';

/// Production wiring. The UI continues to depend only on DiaryApi.
Future<DiaryApi> createProductionDiaryApi() async {
  if (!Platform.isWindows && !Platform.isAndroid) {
    throw const ProductionCoreUnavailable('此安装包目前只支持 Windows 和 Android。');
  }

  final coreLibraryPath = Platform.isWindows
      ? path.join(File(Platform.resolvedExecutable).parent.path, 'diary_bridge.dll')
      : 'libdiary_bridge.so';
  if (Platform.isWindows && !File(coreLibraryPath).existsSync()) {
    throw const ProductionCoreUnavailable('安装包缺少本地资料库组件，请重新安装应用。');
  }

  final supportDirectory = await getApplicationSupportDirectory();
  final libraryDirectory = Directory(
    path.join(supportDirectory.path, 'unwritten_diary'),
  );
  await libraryDirectory.create(recursive: true);

  // The contract requires an IANA zone, not DateTime.timeZoneName (which may
  // be a localized Windows label or an ambiguous abbreviation such as CST).
  final timeZone = (await FlutterTimezone.getLocalTimezone()).identifier;
  if (timeZone.isEmpty) {
    throw const ProductionCoreUnavailable('无法读取设备时区，暂时不能建立资料库。');
  }

  return BridgeDiaryApi(
    coreLibraryPath: coreLibraryPath,
    libraryPath: path.join(libraryDirectory.path, 'library.sqlite'),
    timeZone: timeZone,
    utcOffsetMinutes: DateTime.now().timeZoneOffset.inMinutes,
  );
}

class ProductionCoreUnavailable implements Exception {
  const ProductionCoreUnavailable(this.message);

  final String message;
}
