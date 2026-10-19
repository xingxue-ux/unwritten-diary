# App-specific R8 rules for the release build.
#
# Flutter's Gradle plugin already adds $FLUTTER_ROOT/packages/flutter_tools/gradle/flutter_proguard_rules.pro
# and AGP's proguard-android-optimize.txt. Do not disable minification just to
# "make release work": R8 is part of the size and attack-surface story.
#
# flutter_rust_bridge crosses the boundary through dart:ffi, not JNI
# reflection, so no extra keep rules are required for the Rust bridge itself.
# Plugins that do use Java/Kotlin reflection must add their own rules here or
# in their AAR consumer rules.
#
# Keep this file even when empty: its presence is wired up by Flutter's Gradle
# plugin and gives future Java/Kotlin additions one obvious place to land.
