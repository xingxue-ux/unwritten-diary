import 'models_diary.dart';

/// 给服务配置写密钥的那一步。
///
/// 契约里的 `DiaryApi.saveProvider` **不带密钥**——按最终口径，平台层用系统凭据
/// 接口存好、核心只拿 `credentialRef`（`PlatformHost.storeCredential`）。平台层
/// 的凭据桥接还没接上，所以桥给设置页留了这一条能用的路：明文只在这一次调用里
/// 经过桥，核心写进密钥托管后只留引用。
///
/// 单独一个接口而不是加在 `DiaryApi` 上：`contract_surface_test` 不允许契约之外的
/// 方法混进 `DiaryApi`，而这一条是**过渡期**能力，宿主凭据接上后会被移除。
abstract interface class ProviderCredentialPort {
  /// 给已经存在的服务配置写一次密钥。返回脱敏后的配置（只有引用，没有明文）。
  Future<Provider> saveProviderCredential({
    required String providerId,
    required String secret,
  });
}
