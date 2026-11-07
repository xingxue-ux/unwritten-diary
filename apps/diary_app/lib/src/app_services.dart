import 'dart:convert';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:flutter_timezone/flutter_timezone.dart';
import 'package:path/path.dart' as path;
import 'package:path_provider/path_provider.dart';

import 'bundled_models.dart';

/// 「下次打开哪个资料库」的选择文件。
///
/// 恢复备份会生成一个**新库**，但应用启动时默认只打开应用支持目录下那一个。
/// 用户恢复完、重开应用仍然回到旧库——恢复等于白做。这个文件记住用户的选择，
/// 启动时优先用它；不存在（或指向的库没了）就回到默认路径。
const _libraryChoiceFileName = 'library-choice.json';

/// 应用支持目录下的默认资料库目录。
Future<Directory> defaultLibraryDirectory() async {
  final supportDirectory = await getApplicationSupportDirectory();
  return Directory(path.join(supportDirectory.path, 'unwritten_diary'));
}

File _libraryChoiceFile(Directory supportDirectory) =>
    File(path.join(supportDirectory.path, _libraryChoiceFileName));

/// 读「用户选过的资料库」。
///
/// 只在**文件存在且库文件还在**时返回路径：恢复出来的库被删掉之后，启动应该
/// 回到默认库，而不是打不开。
Future<String?> readChosenLibrary() async {
  try {
    final supportDirectory = await getApplicationSupportDirectory();
    final file = _libraryChoiceFile(supportDirectory);
    if (!await file.exists()) return null;
    final decoded = jsonDecode(await file.readAsString());
    if (decoded is! Map<String, dynamic>) return null;
    final chosen = decoded['libraryPath'];
    if (chosen is! String || chosen.isEmpty) return null;
    return await File(chosen).exists() ? chosen : null;
  } on Object {
    return null;
  }
}

/// 记住用户选的资料库。传 `null` 表示回到默认库。
Future<void> writeChosenLibrary(String? libraryPath) async {
  final supportDirectory = await getApplicationSupportDirectory();
  final file = _libraryChoiceFile(supportDirectory);
  if (libraryPath == null) {
    if (await file.exists()) await file.delete();
    return;
  }
  await file.writeAsString(jsonEncode({'libraryPath': libraryPath}));
}

/// Production wiring. The UI continues to depend only on DiaryApi.
///
/// [libraryPath] 为空时用默认路径（应用支持目录下的 `unwritten_diary/library.sqlite`）。
Future<DiaryApi> createProductionDiaryApi({String? libraryPath}) async {
  if (!Platform.isWindows && !Platform.isAndroid) {
    throw const ProductionCoreUnavailable('此安装包目前只支持 Windows 和 Android。');
  }

  final coreLibraryPath = Platform.isWindows
      ? path.join(File(Platform.resolvedExecutable).parent.path, 'diary_bridge.dll')
      : 'libdiary_bridge.so';
  if (Platform.isWindows && !File(coreLibraryPath).existsSync()) {
    throw const ProductionCoreUnavailable('安装包缺少本地资料库组件，请重新安装应用。');
  }

  final libraryDirectory = await defaultLibraryDirectory();
  await libraryDirectory.create(recursive: true);
  // 随包模型：把语义检索的路径登记进 core-config.json（转写那一路 core 自己按约定找）。
  await BundledModels.ensureConfigured(libraryDirectory);

  // The contract requires an IANA zone, not DateTime.timeZoneName (which may
  // be a localized Windows label or an ambiguous abbreviation such as CST).
  final timeZone = (await FlutterTimezone.getLocalTimezone()).identifier;
  if (timeZone.isEmpty) {
    throw const ProductionCoreUnavailable('无法读取设备时区，暂时不能建立资料库。');
  }

  return BridgeDiaryApi(
    coreLibraryPath: coreLibraryPath,
    libraryPath: libraryPath ??
        path.join(libraryDirectory.path, 'library.sqlite'),
    timeZone: timeZone,
    utcOffsetMinutes: DateTime.now().timeZoneOffset.inMinutes,
  );
}

class ProductionCoreUnavailable implements Exception {
  const ProductionCoreUnavailable(this.message);

  final String message;
}
