-keep class dev.dioxus.main.** { *; }
-keep class com.discordia.app.DioxusForegroundService { *; }
-keep class com.discordia.app.DioxusPlaybackCapture { *; }
-keep class com.discordia.app.DioxusDmNotifications { *; }
-keep class com.discordia.app.DioxusScreenPreview { *; }
-keep class livekit.org.webrtc.** { *; }
-keep class livekit.org.jni_zero.** { *; }
# The SDK omits the stub used only by the unused Java class-loader setter; native init supplies the loader.
-dontwarn livekit.org.jni_zero.JniZeroJni
