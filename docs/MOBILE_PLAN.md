# Discordia Mobile Plan (Android & iOS)

Documento vivo de memoria y seguimiento para la compatibilidad móvil de Discordia con Rust y Dioxus **0.7.10** (Cargo.lock). Revalidado el 2026-10-22.

**Android — Fase 3 en curso.** Keystore, persistencia y chat probados físicamente. Se corrige el cierre nativo de voz (§12 A); después continuar B–F. El usuario autorizó continuar hasta dejar Android funcionando. iOS queda a cargo del colaborador con macOS. Las secciones anteriores conservan decisiones históricas: prevalece el estado de §12.

---

## 1. Estado de Fases

| Fase | Título | Estado | Notas |
|---|---|---|---|
| **Fase 0** | Auditoría y Viabilidad | **COMPLETADA / REVALIDADA** | Inventario y bloqueos verificados en fuentes locales; resultados en §6. |
| **Fase 1** | Build Mobile Base | **ANDROID BASE PROBADA / iOS PENDIENTE** | Check ARM64, APK, identidad y persistencia Keystore en Moto g15 aprobados. iOS requiere macOS. |
| **Fase 2** | UI Responsiva | **BASE VALIDADA / AUDITORÍA RESTANTE** | Ajustes, shell horizontal, emojis y teclado probados físicamente (§12); faltan menús secundarios y pantallas sin adaptar. |
| **Fase 3** | Funciones de Plataforma | **EN CURSO — ANDROID** | Keystore y directorio privado probados; faltan voz/permisos, ciclo de vida y otras adaptaciones. iOS delegado a otro colaborador. |
| **Fase 4** | CI y Documentación | **PENDIENTE** | Workflows GitHub Actions para Android/iOS, guía de compilación. |

---

## 2. Decisiones de Arquitectura Tomadas

1. **Monorepo y Código Único**: UI (`Dioxus 0.7`), protocolo (`discordia-protocol`) y estado del cliente (`client/src/state.rs`, `net.rs`) se mantienen 100% compartidos. Solo se aíslan con `#[cfg(...)]` o features las APIs específicas del sistema operativo.
2. **Estructura Crate Dual (lib + bin)**:
   - `client/Cargo.toml` define `[lib]` con `crate-type = ["cdylib", "rlib"]` y `[[bin]] name = "Discordia"`.
   - `client/src/lib.rs` aloja el árbol de módulos del cliente y exporta `pub fn run()`. Se eliminó su exportación manual de `main`: duplicaba la entrada del binario al enlazar con dx. El ejecutable existente delega en `run()` y Dioxus conserva su trampoline JNI.
   - `client/src/main.rs` es el lanzador de escritorio ligero que delega en `discordia::run()`.
3. **Arranque Responsivo (Desktop vs Mobile)**:
   - En desktop: se configuran dimensiones de ventana fija/mínima (1440x900 / min 1024x600), iconos nativos y menús.
   - En mobile (`android` / `ios`): se omiten tamaños fijos y menús de ventana; la vista webview ocupa el 100% del viewport nativo configurado en `desktop.html` (`width=device-width, initial-scale=1.0`).
4. **Pantalla Inicial (First Boot)**:
   - `app::App` evalúa el estado de `Identity::load()`. Al iniciar en un dispositivo/emulador nuevo sin cuenta, renderiza de forma predeterminada `IdentitySetupView` (generación / importación de clave Nostr BIP-340).
5. **Screenshare en Mobile — propuesta pendiente**: Deshabilitar inicialmente la publicación de pantalla; la captura nativa solo existe en Windows/macOS. La recepción debe reutilizar la vista inline, sin imponer la identidad `#viewer` de la ventana externa desktop. Android/iOS tienen caminos en el build de libwebrtc: no se ha demostrado una imposibilidad general de la biblioteca.
6. **Host / Servidor Embebido — propuesta pendiente**: Deshabilitar auto-alojamiento y mapeo de puertos en móvil, manteniéndolos en desktop. Actualmente `discordia-server` sigue siendo una dependencia incondicional y `net.rs` importa su watchdog. `LIVEKIT_BUNDLE_SKIP=1` solo omite el binario SFU, no elimina el servidor ni su dependencia.
7. **Almacenamiento y Secretos**:
   - iOS: Usa `keyring` con backend `apple-native` (Security framework / Keychain).
   - Android: Keystore pendiente. `vault.key` contiene la contraseña local en texto, no un secreto protegido por Keystore. Además, `keyring 3.6.3` selecciona su backend **mock** en Android: una prueba exitosa de lectura/escritura no demuestra persistencia y puede llevar a elegir incorrectamente `Backend::Keychain`.
   - Config path: seleccionar explícitamente el directorio privado persistente del sistema móvil. El fallback actual a `temp_dir()` no es una solución para identidades.
8. **Portapapeles — propuesta pendiente**: Aislar `arboard` en desktop. Evaluar portapapeles del WebView o bridge nativo, verificando permisos, gestos y soporte real; `navigator.clipboard` no está implementado ni validado en móvil.

---

## 3. Guía de Setup y Comandos para Compilar en Mobile

**Borrador heredado, no validado con dx 0.7.10.** No ejecutar estos pasos como garantía de un build funcional: falta verificar el esquema `[bundle.android]` / `[bundle.ios]`, la selección de features y los comandos de la CLI. Estado real del equipo en §6. No se instalaron herramientas durante esta auditoría.

### A. Prerrequisitos de Rustup
Instalar los targets para arquitecturas móviles:
```powershell
# Android (ARM64 para dispositivos modernos y emuladores, x86_64 para emuladores Intel)
rustup target add aarch64-linux-android x86_64-linux-android

# iOS (Dispositivos físicos y simuladores)
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
```

### B. Herramientas CLI
Instalar la versión fija de Dioxus CLI correspondiente a Dioxus 0.7:
```powershell
cargo install dioxus-cli --version 0.7.10 --locked
cargo install cargo-ndk
```

### C. Configuración Android (SDK / NDK)
1. Instalar Android Studio con **Android SDK Platform 34+**, **Android SDK Command-line Tools** y **NDK (Side by side)** (recomendado NDK r25b o superior).
2. Configurar variables de entorno:
```powershell
$env:ANDROID_HOME = "C:\Users\<Usuario>\AppData\Local\Android\Sdk"
$env:NDK_HOME = "$env:ANDROID_HOME\ndk\<version>"
$env:PATH += ";$env:ANDROID_HOME\platform-tools;$env:ANDROID_HOME\cmdline-tools\latest\bin"
```

### D. Ejecución en Emulador
Para levantar la app en el emulador Android con recarga en caliente de Dioxus:
```powershell
# Iniciar emulador (desde AVD Manager de Android Studio o por CLI)
emulator -avd Pixel_7_API_34

# Ejecutar el cliente con Dioxus CLI
cd client
dx serve --android
```

Para empaquetar APK de desarrollo/release:
```powershell
dx bundle --android
```

---

## 4. Matriz de Auditoría: Funcionalidad vs Mobile

| Funcionalidad | Ubicación en Código | ¿Funciona en Mobile? | Qué hay que cambiar |
|---|---|---|---|
| **Ventana principal y arranque** | `client/src/lib.rs` / `main.rs`, `app.rs` | Parcial; no compila como móvil todavía | Resolver imports desktop, atributo de exportación y verificar arranque real. |
| **Ventanas múltiples (Social / Streams popout / Activities)** | `features/social_window.rs`, `stream_viewer.rs`, `activity_window.rs` | Sin adaptar | Reutilizar vistas inline o deshabilitar ventanas externas. Las actividades deben conservar el aislamiento de IPC/WebRTC de CLAUDE.md, trampa 35; no migrarlas a un iframe/modal en el WebView principal sin ese aislamiento. |
| **Bandeja del sistema (Tray Icon)** | `client/src/app.rs` | ❌ Desktop only | Aislado bajo `#[cfg(target_os = "windows")]`. |
| **Atajos de teclado / Eventos de ratón** | `features/screenshare.rs`, `stream_viewer.rs` (`use_wry_event_handler`) | ❌ Desktop only | Reemplazar escucha de teclas físicas (`Escape`, mouse buttons) por gestos táctiles y botón atrás del sistema en mobile. |
| **Captura de pantalla (Screenshare Tx)** | `client/src/features/screenshare.rs`, `sysvideo/` | Sin captura nativa móvil | Deshabilitar salida con cfg limpio; revisar también la ruta JS `getDisplayMedia` y sus controles. Mantener recepción inline. |
| **Recepción de audio y voz (Voice Rx/Tx)** | `client/src/features/voice.rs`, `audio_device.rs`, `native_sounds.rs` | Bibliotecas con soporte; aplicación sin comprobar | CPAL 0.18.2 usa NDK/AAudio en Android y backend Apple en iOS. Falta integración del contexto Android en WebRTC, permisos en tiempo de ejecución, sesión de audio iOS, rutas Bluetooth y recuperación ante interrupciones. |
| **Cancelación de ruido (DSP / DeepFilterNet / RNNoise)** | `client/src/denoise.rs`, `features/voice_dsp.rs` | Sin compilar ni medir en móvil | Conservar pipeline compartido; verificar tract/C++/ARM y medir CPU/latencia/batería antes de cambiar el modelo predeterminado. |
| **Detección de juegos (Presence)** | `client/src/presence/` | ❌ Desktop only | Deshabilitar en mobile bajo `#[cfg]`: el sandbox de Android e iOS prohíbe enumerar procesos de otras aplicaciones. |
| **Portapapeles (Copy/Paste text & image)** | `client/src/clipboard.rs`, `chat_image.rs`, `text_menu.rs` (`arboard`) | Backend incompatible | Mover dependencia/call sites desktop detrás de cfg y validar alternativa móvil; soporte WebView no garantizado. |
| **Selector de archivos (File Dialogs)** | `features/chat.rs`, `chat_tools.rs`, `identity_setup.rs` (`rfd`) | ❌ `rfd` es desktop | Usar selector HTML (`<input type="file">`) a través del WebView o bridge nativo. |
| **Directorio de configuración / Archivos** | `client/src/identity.rs`, `settings.rs`, caches y grabaciones | Ruta móvil sin validar | Configurar almacenamiento privado persistente antes de migrar/cargar identidad y logs; probar supervivencia a reinicio/actualización. |
| **Bóveda de claves (Keyvault)** | `client/src/keyvault.rs` (`keyring`) | iOS tiene backend; Android usa mock | Integrar Keystore en Android, verificar Keychain en iOS y evitar fallback silencioso a archivo/mock. NIP-49 compartido se conserva. |
| **Auto-actualizador (Updater)** | `client/src/update.rs` | ❌ Desktop only | Deshabilitar bajo `#[cfg(not(any(target_os = "android", target_os = "ios")))]`. |
| **Auto-alojamiento de servidor (Host)** | `client/src/host.rs`, `discordia-server` | ❌ Desktop only | Deshabilitar `host.rs` en mobile. La app móvil es puramente cliente. |
| **Diseño / Layout UI** | `client/src/features/workspace.rs`, `app.rs` | Implementación parcial, sin prueba móvil | Validar navegación, targets táctiles, notch, rotación y teclado real; conservar componentes únicos. |
| **Cámara** | `features/camera.rs`, `syscamera.rs`, `features/screenshare.rs` | Permiso implementado; captura sin validar | `CAMERA` declarado y solicitado en ejecución; la ruta WebView espera la clave del canal antes de publicar. Captura y preview pendientes de prueba física; la captura nativa Windows no se reutiliza automáticamente. |
| **Notificaciones y sonidos** | `native_sounds.rs`, `features/workspace.rs` | Sonidos CPAL; entrega en segundo plano sin comprobar | Separar sonidos en primer plano de notificaciones del sistema; no prometer push/llamadas con app suspendida. |
| **JS / WebView** | `features/screenshare.rs`, `features/chat.rs`, `activity_window.rs`, `assets/*` | Parcial | Se usa `document::eval`, no un cliente WASM con web-sys directo. Comprobar WebRTC, E2EE worker, codecs, reproducción y selección de salida en Android WebView/WKWebView. |
| **Ciclo de vida y reconexión** | `net.rs`, `features/workspace.rs`, `features/voice.rs` | Reconexión de red existente; resume móvil pendiente | Revisar hooks reales de Dioxus/Tao 0.7.10 y reconectar sin sesiones duplicadas al volver del fondo. |
| **Actualización / apertura externa / instancia única** | `update.rs`, `single_instance.rs`, `webview2.rs` | Específico de desktop | Instancia única/WebView2 ya son Windows; aislar flujo del actualizador y lanzadores externos para móvil. |
| **Auto-host / UPnP / NAT-PMP / túnel SFU** | `host.rs`, `portmap/`, `sfu_tunnel.rs`, `net.rs` | Auto-host sin adaptar | Aislar funciones del host, conservando transporte de cliente QUIC/WS y el túnel de señalización necesario para unirse a hosts. |

---

## 5. Fase 2 — UI Responsiva (registro de cambios)

**Hallazgo:** la Fase 2 ya estaba esbozada en `workspace.rs` pero **el workspace no compilaba** (`error: expected identifier` en `workspace.rs:711`); la afirmación de Fase 1 "COMPLETADA" era prematura. Se corrigió y completó:

| Cambio | Archivo | Por qué |
|---|---|---|
| Shell móvil de una columna con tabs | `client/src/features/workspace.rs` | `if is_mobile() { rsx!{…} } else { rsx!{…} }` no es rsx válido dentro del `rsx!` padre; los nodos se inlinaron en el `if`/`else`. |
| Clase activa del nav sin `if` inline | `client/src/features/workspace.rs` | Un `{if … { "a" } else { "b" }}` dentro de `class:` no parsea ("Expected Ident or Expression"); se extrajo a la closure `mobile_nav_class(MobileTab)`. |
| Target táctil del botón "Back to chat" | `client/src/features/workspace.rs` | `min-h-[44px] min-w-[44px]` no está en `client/assets/tailwind.css` (no se regenera sin `dx`); se usa `dxf-touch-target`, definido en `BASE_CSS`. |

**Ya presente y verificado (sin cambios):** detección de breakpoint (`matchMedia('max-width: 768px')` + `cfg!`), safe areas (`--safe-*` con `env(safe-area-inset-*)` + `padding` en el shell), nav inferior con `.dxf-mobile-nav button` a ≥44px, teclado (`interactive-widget=resizes-content` y `viewport-fit=cover` en `desktop.html`), pantalla de identidad de una columna (`hidden md:flex`), diálogo de ajustes responsive (`settings.css` + media queries a 720/480px), y reutilización de `GuildsSidebar`/`ChannelsColumn`/`ChatView`/`MembersPanel`.

**Verificación:** `cargo check --workspace` → exit 0, sin warnings de Rust (solo el log de descarga del SFU de `discordia-server`). Los `warning: unused import` previos eran artefactos del macro rsx roto y desaparecieron.

**Riesgos / no comprobado:**
- No hay `dx` instalado en esta máquina: `client/assets/tailwind.css` no se regeneró; las clases móviles se apoyan en `BASE_CSS` (`dxf-*`) para no depender de `dx`.
- El check de escritorio valida las ramas de UI seleccionadas con `cfg!`, pero **no** dependencias ni código bajo `#[cfg(target_os = "android"/"ios")]`. El intento Android del 2026-10-22 falla antes de compilar el cliente (§6); iOS no se compiló.
- Sin validación en dispositivo/emulador.

---

## 13. Pantalla compartida en Android — plan (Fase 3 pendiente, sesión 23/10)

**Estado verificado del árbol (23/10/2026):** la migración está casi completa. Hay integración Android real, no simulada:

| Pieza | Dónde |
|---|---|
| Alias de renderer | `client/src/lib.rs` (`dioxus::desktop`/`dioxus::mobile` como `crate::platform`) |
| Puente JNI | `client/src/android.rs`: `initializeWebRtc` (`Java_dev_dioxus_main_MainActivity_initializeWebRtc`), permisos de mic/cámara, `vault.key` + passphrase, portapapeles, `document()` (selector de archivos) |
| Activity nativa | `client/android/MainActivity.kt` (`WryActivity`, permisos, file picker, clipboard, `softInputMode=ADJUST_RESIZE`) |
| Empaquetado | `scripts/build-android.py` (inyecta `libwebrtc.jar`, branding, verifica símbolos JNI), `scripts/setup-android.py`, `scripts/android-env.ps1` |
| Build reproducido | `dx 0.7.10 (57d6794)`, APK debug 727 MB en `target/dx/Discordia/debug/android/app/app/build/outputs/apk/debug/` |

**Lo único pendiente: publicar pantalla (compartir) desde Android.**
- `client/src/sysvideo/` solo tiene `windows.rs` y `macos.rs`; `sysvideo::supported()` es `false` en Android.
- El share del cliente se apoya en el WebView: `features/screenshare.rs` sondea `navigator.mediaDevices.getDisplayMedia` y, si no existe, muestra *"Screen sharing isn't available in this build's webview."* **Android WebView no implementa `getDisplayMedia`**, por eso no anda.
- Ver streams (recepción) sí funciona; el que falla es el envío.

**Piezas ya disponibles para implementarlo:**

| Pieza | Dónde |
|---|---|
| `org.webrtc.ScreenCapturerAndroid` (usa MediaProjection) | ya empaquetado en el `libwebrtc.jar` del APK |
| Empujar frames a un source desde Rust | `webrtc-sys::video_track::VideoTrackSource::{new_video_track_source, on_captured_frame}` (`vendor/webrtc-sys/src/video_track.rs:114,133`) |
| Conversión a I420 | `vendor/webrtc-sys/src/video_frame_buffer.rs`, `yuv_helper.rs` |
| Patrón JNI `on_ui`/`with_env` | `client/src/android.rs` |
| Ruta de publicación de video de la sala | `features/voice.rs` (`VoiceCmd::SetScreenVideo`) + `livekit` (`LocalVideoTrack`, `NativeVideoSource`) |

**Plan de implementación (feature mediana, aún NO hecha):**
1. **Kotlin** (`MainActivity.kt`): pedir `MediaProjection` con `startActivityForResult`, crear `ScreenCapturerAndroid` con un `CapturerObserver` propio que por cada `VideoFrame` extraiga I420 y llame a un `external fun onScreenFrame(...)` (JNI); exponer `startScreenShare()/stopScreenShare()`.
2. **Rust** (`android.rs`): JNI `start_screen_share()`/`stop_screen_share()` + callback `Java_dev_dioxus_main_MainActivity_onScreenFrame` que arme un `I420Buffer` (`webrtc_sys`) y llame a `on_captured_frame` sobre el `VideoTrackSource` publicado; guardar ese source en estado global.
3. **Rust** (`voice.rs`/`screenshare.rs`): variante móvil de `SetScreenVideo` que publique ese source en vez de un `sysvideo::Target`; `screen_capture_available = true` en Android.
4. **UI**: habilitar el botón de compartir en mobile y omitir el gate del WebView.

**Cómo dejar el entorno listo (Windows, toolchain repo-local):**
```powershell
cd C:\Users\Recko\Documents\github\discordia
$b="$PWD\target\mobile-tools"; $sdk="$b\android-sdk"; $ndk="$sdk\ndk\27.2.12479018"
$ndkbin="$ndk\toolchains\llvm\prebuilt\windows-x86_64\bin"; $java="$b\java\jdk-21.0.9+10"
$env:ANDROID_HOME=$sdk; $env:ANDROID_SDK_ROOT=$sdk; $env:ANDROID_NDK_HOME=$ndk; $env:JAVA_HOME=$java
$env:PATH="$ndkbin;$java\bin;$sdk\platform-tools;$b\dioxus;$env:PATH"
adb devices                      # ZY32L4M4NQ, Moto G15 -> 'device' (autorizado)
python scripts/build-android.py --install   # compila + instala el APK
```

**Bloqueadores/notas:**
- PowerShell bloquea scripts (`ExecutionPolicy`); usar `Set-ExecutionPolicy -Scope Process Bypass` o el env inline de arriba (`scripts/android-env.ps1` usa `$PSScriptRoot`, que llega vacío cuando se corre en línea).
- Build Android: ~700 MB de APK y minutos por vuelta; conviene iterar con `dx serve --android` si se puede, o `build-android.py` para instalación completa.

## 14. Captura nativa de pantalla y cámara en Android (2026-10-23)

Implementa el plan de §13 y lo extiende a la cámara: **la publicación es 100% nativa** (Rust + JNI), sin `getDisplayMedia` ni `getUserMedia` del WebView. Ver streams y la propia cámara sigue por el WebView, como en desktop.

| Pieza | Dónde |
|---|---|
| Capturador de pantalla (MediaProjection) | `client/android/MainActivity.kt`: consentimiento por `startActivityForResult`, `ScreenCapturerAndroid` + `SurfaceTextureHelper`/`EglBase`, observador propio |
| Capturador de cámara | `client/android/MainActivity.kt`: `Camera2Enumerator`, `createCapturer`, mismo observador |
| Empuje de frames | `external fun onVideoFrame(...)` → `Java_dev_dioxus_main_MainActivity_onVideoFrame` (`client/src/android.rs`); los planos I420 se leen como `ByteBuffer` directos y se copian a `I420Buffer` |
| Registro por slot | `client/src/android.rs` (`CAPTURE_SINKS`): cada captura tiene un slot, así un capturador viejo no puede publicar en la siguiente |
| Backend nativo | `client/src/sysvideo/android.rs` (`sources`/`start`/`Capture`), módulo `android` de `client/src/syscamera.rs` (`devices`/`start`) |
| Publicación | sin cambios: `features/voice.rs` (`ScreenVideoRoom`, `CameraPublisher`), con los `#[cfg]` extendidos a Android |
| Servicio en primer plano | `client/android/DioxusForegroundService.kt` + `[android] foreground_service_types` en `client/Dioxus.toml` |
| Empaquetado | `scripts/build-android.py`: dos pasadas de `dx` (la primera puede parar donde todavía falta el jar de WebRTC), `client/android/webrtc.dx.init.gradle` auto-aplicado desde `$GRADLE_USER_HOME/init.d/`, copia del servicio y verificación de clases y símbolos JNI nuevos |

Decisiones:

- **Android 14 exige un FGS de tipo `mediaProjection`** antes de crear el display virtual. Orden actual: consentimiento → servicio confirmado → capturador; parar el share conserva otros medios activos.
- `sysvideo::supported()` y `syscamera::supported()` son `true` en Android: la UI entra por la ruta nativa sin tocar `screenshare.rs`/`camera.rs` más allá del picker.
- El selector de fuentes se salta en Android (`open_screen_picker` va directo a `choose_source` con `Target::AndroidScreen`): no hay miniatura sin arrancar la proyección.
- El diálogo de cámara no previsualiza: la cámara no se puede abrir dos veces y el WebView retendría el dispositivo que necesita la publicación. La vista propia sale de la pista publicada.
- El permiso de cámara se pide al arrancar la captura (`toggle_camera`), no al entrar a un servidor: `refresh_cameras` solo enumera, y enumerar no lo necesita.
- El display virtual se dimensiona al aspecto actual de la pantalla dentro del presupuesto del share (`captureSize`), en vez de recortar a 16:9.
- El observador **no** libera el `VideoFrame`: lo libera el capturador (`SurfaceTextureHelper` justo después de `onFrame`, `Camera2Session` después de la suya). Liberarlo aquí mata el hilo `discordia-camera` con `release() called on an object with refcount < 1`.
- Los planos que entrega `YuvConverter` no son I420 "planos": U y V van entrelazados dentro de cada fila de stride de luma, así que la última fila del plano es media fila. Exigir `stride*rows` descarta **todos** los frames; la copia (`copy_i420_plane_into`, con test) usa `stride*(rows-1) + row_len`.
- La cámara se abre **antes** de publicar la pista, así que el adaptador de libwebrtc todavía no tiene consumidor y descarta esos frames (`capture_frame` devuelve `false`). El "primer frame" se señaliza sin mirar el retorno, como en Windows; esperar a `true` daba un timeout de 12 s y apagaba la cámara.
- La rotación que reporta el capturador viaja en `Frame.rotation` hasta `VideoFrame`; el share de pantalla va en `VideoRotation0`. Un display virtual conserva el tamaño con el que se creó, así que girar el teléfono a mitad del share deformaba la captura: `MainActivity.onConfigurationChanged` recalcula `captureSize` y llama `ScreenCapturerAndroid.changeCaptureFormat`, que lo recrea (el manifiesto ya declara `orientation|screenLayout|screenSize`, así que la Activity no se recrea).
- **El paso sin copia de la cámara no se hizo**, y no por falta de sitio: `webrtc::jni::JavaToNativeFrameBuffer` (headers disponibles en el archivo de WebRTC) permitiría envolver el buffer Java sin copiar, pero para la cámara `InternalSource::on_captured_frame` llama `ToI420()` igual porque hay rotación (el readback se movería de Java a C++, no desaparecería), y para la pantalla el encoder en modo surface exigiría compartir el contexto EGL entre capturador y encoder, cosa que la fábrica nativa de libwebrtc no garantiza. Lo único que se ahorraría es nuestra copia de planos (~1–2 ms/cuadro), que no paga el riesgo de una referencia JNI mal llevada.
- El APK solo permitía cleartext a `127.0.0.1` (config de dx), así que la sala WebView no podía unirse a un SFU `ws://` fuera de loopback y salía *"Couldn't connect to the screen stream… Failed to fetch"* — publicar andaba, ver no. `client/android/res/xml/network_security_config.xml` lo permite, como el escritorio con su flag de origen inseguro.
- La captura nativa a 60 fps satura el encoder de un móvil (24 ms/cuadro medido en un Moto G15, `encode usage` 102–117%, cola llena y descartes): en Android se limita a 30 fps. Y como el display virtual de MediaProjection refresca a la tasa de la pantalla, el capturador entrega 60 fps igual: `FramePacer` (compartido con Windows en `sysvideo/pacing.rs`) los baja a la tasa pedida. Sin ese pacing el emisor reportaba "captura 61 fps / codificado 30 fps" y al receptor le salía el aviso de "limitación de captura o codificación en su equipo", que era falso.
- El coste por cuadro sigue al tamaño **capturado** (readback de GPU en `toI420` + copia), no al codificado, así que bajar la resolución de codificación no lo reduce: con el preset 1080p en apaisado la captura era 1920×852 y el `encode usage` 134–139%. `mobile_capture_budget` limita la captura a 720p30 en móvil y recalcula el `max_bitrate` con `upload_budget`.
- tao responde `false` a `Window::is_focused` y `Window::is_visible` en Android, así que el cliente creía tener su ventana oculta: la vista previa quedaba en "pausada con la app en segundo plano" y la sala WebView se marcaba invisible (sin decodificar). `screenshare::window_active`/`window_shown` devuelven `true` en móvil.
- El botón de compartir pantalla se ponía en "compartiendo" 160 ms después de pulsarlo, **antes** del diálogo de consentimiento, y se revertía si lo cancelabas (en escritorio no se nota porque no hay diálogo). Ahora `android::request_screen_capture()` pide el permiso primero y `choose_source` corre solo tras aceptarlo, así que el estado local y el `SetScreenShare` del servidor salen cuando la captura ya va a arrancar. Medido: cámara abre el diálogo en 107 ms, enciende en 2.7 s (con "Iniciando tu cámara…") y apaga en 101 ms.
- **`dx build` ejecuta Gradle él mismo**, antes de que el script pueda copiar `libwebrtc.jar`, así que la primera pasada puede parar en `compileDebugKotlin`. `build-android.py` la tolera, copia el jar e instala `webrtc.dx.init.gradle` en `$GRADLE_USER_HOME/init.d/` — Gradle lo aplica solo y dx no acepta `--init-script` — y la segunda pasada, que sí debe pasar, ya compila el Kotlin contra las clases de WebRTC. El script lo limita a proyectos que ya tienen `libs/libwebrtc.jar`, así que no toca otros builds.

Probado en el Moto G15 el 23/10: pantalla y cámara publican y otro cliente las ve. Con pantalla sola y el presupuesto móvil: `encode usage` 57.9% (máx 59%), 0 descartes por cola llena, 0 errores de la sala WebView, y `Enviando: 344×720 · 29 FPS · captura 29 FPS`. La rotación durante el share se probó girando el teléfono (`user_rotation`): la captura pasa de 346×720 a 1280×568 y el encoder se reconfigura, y vuelve al girar de nuevo. La cámara sola sigue siendo lo más pesado del equipo (readback de 1280×720 + copia), y libwebrtc la adapta sola; ver arriba por qué el paso sin copia no compensa.

## 6. Revalidación de Fase 0 — 2026-10-22

### Alcance y estado recibido

- Rama recibida: `master`, con cambios locales previos en `client/Cargo.toml`, `client/Dioxus.toml`, `client/assets/desktop.html`, `client/src/app.rs`, `client/src/features/workspace.rs`, `client/src/main.rs`; `client/src/lib.rs` y este plan sin seguimiento Git. Se conservaron todos.
- Archivos modificados en esta revisión: **solo `docs/MOBILE_PLAN.md`**. Sin dependencias nuevas, cambios de código, commits ni instalación en el celular.
- Se leyó primero este plan, luego `CLAUDE.md`, el mapa y fuentes puntuales. Las secciones anteriores se corrigieron donde confundían propuestas con implementación o pruebas de desktop con pruebas móviles.
- Teléfono Android conectado en modo depuración según el usuario. Confirmó que aún no instaló Android Studio/SDK/platform-tools. ADB no disponible en PATH ni en la ubicación habitual: no se pudo enumerar el teléfono.
- Targets Rust instalados: `aarch64-linux-android`, `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-pc-windows-msvc`. Tener el target Rust no instala NDK/Xcode. No se encontró `dx` ni `cargo-ndk` en los binarios Cargo.

### Bloqueos concretos de la base móvil

| Hallazgo | Evidencia local | Acción en Fase 1 |
|---|---|---|
| Namespace incorrecto al compilar solo con feature mobile | Dioxus 0.7.10 `src/lib.rs`: `dioxus::desktop` solo se exporta con feature desktop; mobile exporta `dioxus::mobile`. `app.rs`, `i18n.rs`, ventanas y screenshare siguen importando desktop incondicionalmente. | Adaptar la frontera de plataforma usando exports existentes; no activar ambas features como sustituto de separar funciones desktop. |
| Export Android incompatible con edition 2024 | `client/src/lib.rs:264` utiliza `#[no_mangle]`; el paquete declara edition 2024. | Corregir atributo y comprobar el trampoline real de `vendor/dioxus-desktop/src/mobile.rs`, que busca `main`/`_main`. |
| Ventana de actividades cae en GTK fuera de Windows/macOS | `activity_window.rs`, rama `#[cfg(not(any(target_os = "windows", target_os = "macos")))]`, importa `tao::platform::unix::WindowExtUnix` y `build_gtk`. | Restringir GTK a Linux; deshabilitar actividades externas móviles hasta tener aislamiento equivalente. |
| Dependencias desktop siempre presentes | Árbol Android incluye `arboard`, `rfd`, `discordia-server` y herramientas de auto-host. | Separar dependencias/call sites de plataforma, sin duplicar lógica ni quitar capacidades desktop. |
| Selección de bóveda Android errónea | `keyring 3.6.3/src/lib.rs` elige mock en Android; `src/mock.rs` declara que no persiste ni siquiera entre entradas independientes. `keyvault::keychain_usable` solo prueba una entrada. | No aceptar mock como Keystore; resolver almacenamiento seguro en Fase 3 antes de considerar listas las identidades persistentes Android. |
| Configuración dx no verificada | Los nuevos bloques de `Dioxus.toml` no se probaron con la CLI 0.7.10; el crate runtime `dioxus-cli-config` no certifica el esquema de bundle de la CLI. | Verificar código/documentación de la CLI exacta antes de ajustar manifest/plist o prometer comandos de bundle. |

### Dependencias: incompatibilidad vs falta de validación

Diagnóstico por lectura de las versiones resueltas y su selección de plataforma. No es un resultado de compilación completa para ambos targets; el intento Android se detuvo en el primer requisito de toolchain ausente.

| Dependencia resuelta | Android aarch64 | iOS aarch64 | Decisión / evidencia |
|---|---|---|---|
| `arboard 3.6.1` | Sin backend Android | Selecciona módulo Linux/X11; no backend iOS | `src/platform/mod.rs`; incompatible como dependencia incondicional. Mantener solo en desktop. |
| `rfd 0.17.2` | Sin backend nativo | Sin backend nativo | `src/backend.rs` solo implementa plataformas desktop y WASM. Aislar llamadas de selección/guardado; la alternativa WebView debe probarse. |
| `dioxus 0.7.10` + `dioxus-desktop 0.7.10` parcheado | Tiene renderer/trampoline móvil | Tiene rutas iOS | El framework no es el bloqueo general; sí lo son imports/ventanas de la aplicación. Revisar también los parches locales en targets reales. |
| `keyring 3.6.3` | Compila con mock; **no sirve como bóveda** | Backend `apple-native` selecciona `ios` | `src/lib.rs`, `src/mock.rs`. Integración y persistencia deben probarse; no confundir compilación con protección de claves. |
| `cpal 0.18.2` | Backend NDK/AAudio, dependencia `ndk` con `api-level-26` | Backend Apple de audio | Soporte en fuentes; no compilado aquí. Fijar mínimo Android compatible y gestionar permisos/audio focus/interrupciones. |
| `livekit 0.9.5`, `libwebrtc 0.3.52`, `webrtc-sys 0.3.49`, `webrtc-sys-build 0.3.19` | Build contiene configuración Android/JVM/contexto | Build enlaza frameworks iOS | `vendor/webrtc-sys/build.rs` y `src/android.rs`; falta NDK/artefactos/inicialización y validación. No declararlos incompatibles sin prueba. |
| `ring 0.17.14` y otras dependencias C/C++ (`secp256k1-sys`, WebRTC) | Requieren compilador del NDK | Requieren toolchain/SDK Apple | `ring` es el primer fallo observado en Android por compilador ausente, no un defecto demostrado de ring. |
| `deep_filter` revisión `d375b2d`, `tract-* 0.21.4` | Sin validar | Sin validar | Mantener DSP compartido; verificar kernels ARM/toolchain y rendimiento antes de decidir cambios de modelo. |
| `discordia-server` y SFU empaquetado | Sin SFU precompilado móvil en build.rs | Sin SFU precompilado móvil en build.rs | Dependencia actual también usada por `net.rs` para watchdog; no basta con quitar una línea de Cargo.toml. `LIVEKIT_BUNDLE_SKIP` no elimina el código del servidor. |
| `sysinfo 0.39.6`, `local-ip-address 0.6.13`, `dirs 5.0.1` | No son un fallo general de compilación demostrado | No son un fallo general de compilación demostrado | sysinfo/local-ip-address tienen selección móvil en sus fuentes; eso no habilita escaneo de juegos ni garantiza rutas de almacenamiento correctas. |
| Dependencias Windows/macOS/Linux del cliente | Ya seleccionadas por target para sus OS | Ya seleccionadas por target para sus OS | Windows capture/WebView2, ScreenCaptureKit/CoreAudio y WebKitGTK deben permanecer fuera de móvil; auditar call sites además del manifest. |
| `protocol`, `grid-layout`, transporte y lógica compartida | Sin incompatibilidad confirmada en esta lectura | Sin incompatibilidad confirmada en esta lectura | No reescribir ni duplicar; faltan checks de destino. `server`, `rendezvous`, `bot-sdk` siguen siendo aplicaciones/servicios con sus builds de host; el APK no requiere compilar todos como apps móviles. |

### Verificación ejecutada y límites

```powershell
cargo check --workspace
# exit 0
# warning: discordia-server@0.1.0: downloading ...livekit_1.13.9_windows_amd64.zip
# Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.23s

cargo check -p discordia --lib --no-default-features --features mobile --target aarch64-linux-android --offline
# exit 101
# error: failed to run custom build command for `ring v0.17.14`
# ToolNotFound: aarch64-linux-android-clang / clang.exe

cargo tree -p discordia --target aarch64-linux-android --no-default-features --features mobile --offline --depth 1 -e normal
# exit 0; confirma dependencias incondicionales; no valida su compilación
```

Logs completos fuera del repositorio, en `C:/Users/Recko/.codex/visualizations/2026/10/20/01a157cb-7e6d-7810-89b7-2128accd1321/mobile-desktop-check.log` y `mobile-android-check.log`.

iOS: no compilado ni ejecutado. Este equipo es Windows; la validación de bundle, firma, simulador y dispositivo iOS requiere macOS con Xcode. Tampoco se comprobó el comportamiento móvil de UI, audio, teclado, notificaciones ni persistencia de identidad.

### Próxima fase y criterios de cierre

1. **Fase 1:** instalar/verificar dx 0.7.10 y Android SDK/NDK/JDK/platform-tools; enumerar el teléfono con `adb devices -l`; seleccionar features y corregir bloqueos de base por plataforma; generar APK y abrir identidad Nostr en el teléfono conectado. Documentar versiones y comandos que efectivamente funcionen, con check de desktop aprobado. iOS requiere su validación separada en macOS, no se marca completo por un APK Android.
2. **Fase 2:** comprobar el shell existente en dispositivo, navegación táctil, notch/rotación y teclado; cambios mínimos en componentes/CSS compartidos.
3. **Fase 3:** permisos declarados y solicitados en ejecución; Keystore/Keychain reales; reconexión tras suspensión; captura de pantalla saliente deshabilitada limpiamente. No copiar contraseñas a `vault.key` ni aceptar mock como solución móvil.
4. **Fase 4:** CI de compilación Android y validación iOS en runner macOS; guía con versiones/comandos comprobados.

Fin de esta fase: detenerse tras presentar la auditoría. No comenzar implementación ni instalar herramientas hasta continuar con Fase 1.

## 7. Fase 1 — implementación (2026-10-22)

Rama `mobile-platform-support`, creada conservando los cambios locales de las fases anteriores. El usuario autorizó continuar, aceptar la licencia del SDK y probar en su teléfono. No se publicaron commits ni se modificó `test-and-benchmarks-NoMergear`.

### Herramientas y dispositivo

| Elemento | Versión / estado |
|---|---|
| Dioxus CLI | 0.7.10, binario oficial Windows x64, checksum verificado |
| Java | Temurin 21.0.9+10, ZIP oficial y checksum verificado |
| Android command-line tools | 15859902, checksum del sitio oficial de Android |
| Android platform / build-tools | API 34 / 34.0.0 |
| Android NDK | 27.2.12479018; compilador ARM64 con mínimo API 26 |
| Platform-tools | 37.0.1 |
| Teléfono | Moto g15, Android API 35, arm64-v8a; depuración USB autorizada y ADB en estado `device` |
| Instalación | Local a `target/mobile-tools`; sin modificar PATH global ni instalar Android Studio |

### Archivos y decisiones

| Archivos | Cambio / motivo |
|---|---|
| `client/Dioxus.toml` | Usar `[android]` y `[ios]` de la CLI 0.7.10, mínimo Android 26 y SDK 34. Quitar bloques de bundle incorrectos: `bundle.android` es para claves de firma; `bundle.ios` no existe en ese esquema. |
| `client/src/lib.rs`, `app.rs`, `i18n.rs`, `features/screenshare.rs`, `features/social_window.rs`, `features/stream_viewer.rs`, `activity_window.rs` | Alias `crate::platform` del renderer Dioxus desktop/mobile; misma UI y lógica, sin duplicarlas. Quitar exportación manual `main` tras reproducir el error de enlace por símbolo duplicado. |
| `client/Cargo.toml`, `client/src/file_dialog.rs`, `features/chat.rs`, `features/chat_tools.rs`, `features/identity_setup.rs`, `features/integrations.rs` | Mover `rfd` a targets desktop y centralizar diálogos. En móvil devuelven una limitación explícita; selector nativo/WebView pendiente. |
| `clipboard.rs`, `chat_image.rs`, `text_menu.rs`, `client/Cargo.toml` | Mantener `arboard` solo en desktop y conservar su comportamiento. Botones de portapapeles móviles todavía no disponibles, sin informar éxito falso. |
| `activity_window.rs` | Rechazar apertura de actividades externas en móvil, sin ejecutarlas dentro del WebView principal ni intentar usar GTK. |
| `keyvault.rs` | Android no consulta ni acepta el backend mock y rechaza persistir claves hasta integrar Keystore en Fase 3. No se agregó una bóveda de archivo como sustituto. |
| `client/assets/tailwind.css` | Regenerado por dx 0.7.10 a partir de las fuentes actuales, incluido el shell responsive previo. |
| `docs/MAP.md` | Corregir referencias al arranque/logging: ahora viven en `client/src/lib.rs`, no en el lanzador mínimo `main.rs`. |
| `scripts/setup-android.py`, `scripts/android-env.ps1` | Setup Windows reproducible, descargas verificadas y variables solo para la terminal actual. Python 3.11+ y biblioteca estándar; sin dependencias nuevas de la aplicación. Alternativa: instalar herramientas manualmente mediante Android Studio y pasar `-SdkRoot`/`-JavaRoot`. |

Fuentes de configuración verificadas en el tag `v0.7.10` de Dioxus: `packages/cli/src/config/{app,bundle,manifest}.rs` y `packages/cli/src/build/android.rs`; no se usó documentación de otra versión para inventar campos.

### Comandos Windows

Desde la raíz del repositorio, en PowerShell:

```powershell
# Requiere Python 3.11+. El flag confirma aceptación de la licencia del SDK.
python scripts/setup-android.py --accept-sdk-license
. ./scripts/android-env.ps1
rustup target add aarch64-linux-android
dx --version
adb devices -l

# Solo esta terminal; el móvil no empaqueta un ejecutable de SFU.
$env:LIVEKIT_BUNDLE_SKIP = '1'
cargo check -p discordia --lib --no-default-features --features mobile --target aarch64-linux-android
Set-Location client
dx build --android --target aarch64-linux-android --no-default-features --features mobile --bin Discordia
Set-Location ..
Remove-Item Env:LIVEKIT_BUNDLE_SKIP
cargo check --workspace

# Instalar y abrir el APK generado en el dispositivo USB autorizado.
adb install -r target/dx/Discordia/debug/android/app/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -W -n com.discordia.app/dev.dioxus.main.MainActivity
```

El servidor sigue siendo dependencia incondicional porque el cliente usa tipos/helpers suyos. `LIVEKIT_BUNDLE_SKIP` solo omite su SFU; separar auto-host sigue pendiente, sin prometer una app móvil funcional como servidor.

### Verificación final de esta entrega

- Check Android ARM64: exit 0; compiló CPAL, WebRTC y DSP. 37 advertencias del cliente y 6 del renderer, mayormente código desktop sin uso en móvil; no se ocultan con `-A warnings`.
- Check workspace Windows final: exit 0, `Finished dev profile ... in 26.94s`; solo advertencia del bundle SFU, sin advertencias de Rust en esta ejecución.
- Primer `dx build`: reprodujo `duplicate symbol: main`; corregido eliminando la exportación manual de la biblioteca. Segundo build: exit 0 y APK generado. El daemon Gradle de esa ejecución se cerró al terminar.
- APK debug: `target/dx/Discordia/debug/android/app/app/build/outputs/apk/debug/app-debug.apk`, 349.9 MiB. No es una distribución release ni está optimizado para tamaño.
- `adb install -r`: `Success`. `am start -W`: `Status: ok`, inicio en frío de 3297 ms. Actividad foreground y proceso de Discordia comprobados después del arranque; captura visual confirma la UI de identidad compartida.
- El usuario intentó crear una identidad y vio el error esperado `save failed: Secure identity storage is not available on Android yet.`. Importar la identidad de PC también queda bloqueado: ambas rutas requieren guardar su clave. No se probó acceso a servidores, voz ni persistencia entre reinicios.
- Captura local: `C:/Users/Recko/.codex/visualizations/2026/10/20/01a157cb-7e6d-7810-89b7-2128accd1321/mobile-login.png` (frase oculta por la interfaz).
- Script de setup ejecutado nuevamente contra herramientas instaladas: exit 0. Entorno PowerShell comprobado con `dx --version`; sintaxis Python, `cargo fmt --all -- --check` y `git diff --check` aprobados.
- Logs: `target/mobile-tools/{android-check,desktop-check,dx-android-build,setup-check}.log`.
- iOS: configuración verificada en fuentes 0.7.10, sin compilación, firma ni ejecución; requiere macOS/Xcode. No marcar la fase completa para ambas plataformas.

### Pendientes al detenerse

- Integrar Keystore/Keychain y directorio privado persistente en Fase 3 antes de habilitar creación/importación de cuentas móviles. El bloqueo temporal no debe reemplazarse por archivo plano ni por el mock de keyring.
- Validar build iOS en macOS y cerrar lo que revele ese target. Android físico sustituye al emulador en la prueba de arranque de esta entrega; no se instaló ni se probó un emulador.
- Fase 2 conserva trabajo responsive previo, sin validación completa de teclado/rotación/navegación. Fases 3 y 4 siguen pendientes; en particular diálogos de archivos, botones de clipboard, actividades externas, auto-host, permisos de voz y ciclo de vida.
- No se añadieron dependencias Rust, no cambió Cargo.lock y no se hicieron commits ni publicación.

## 8. Android — identidad persistente y pruebas de conexión (2026-10-22)

### Alcance autorizado

El usuario indicó que otro colaborador validará macOS/iOS y que continuemos con Android. Se adelantó la parte de almacenamiento de Fase 3 para desbloquear creación/importación, sin duplicar la lógica Nostr. Luego autorizó probar y dejó elegir el servidor de prueba.

### Cambios de esta etapa

| Archivo | Decisión / motivo |
|---|---|
| `client/src/android.rs` | Adaptador Rust/JNI para `Context.getNoBackupFilesDir()` y Android Keystore. Carpeta privada `no_backup/discordia`, excluida de copias automáticas que no pueden restaurar la clave Keystore. |
| `client/src/android.rs` | Clave HMAC-SHA256 no exportable de AndroidKeyStore, alias `com.discordia.identity-vault.hmac.v1`, etiqueta fija `discordia/identity-vault/passphrase/v1`. Su resultado deriva la contraseña local del cifrado NIP-49 existente; ni la clave Keystore ni esa contraseña se guardan en archivo. Se comprueba que `getEncoded()` sea null. |
| `client/src/android.rs` | Acceso al vault serializado; referencias JNI locales acotadas y excepciones limpiadas. Si faltara la clave y hubiera estado previo, rechazar en vez de regenerarla silenciosamente. Alias/etiqueta son parte del formato persistente: requieren migración si se cambian. |
| `client/src/lib.rs` | Inicializar almacenamiento Android antes de cargar configuración/logs/identidad. |
| `client/src/identity.rs` | Usar siempre carpeta privada en Android; ignorar override de entorno/carpeta de identidades y rechazar mover claves fuera de ella. Desktop conserva sus rutas y migración. |
| `client/src/keyvault.rs` | Sustituir el bloqueo temporal Android por el adaptador Keystore. Mantener NIP-49 y las APIs del cliente compartidas. |
| `client/src/features/identity_setup.rs` | Ocultar selector de carpeta en Android: la identidad permanece en almacenamiento privado. |
| `client/Cargo.toml`, `Cargo.lock` | Dependencias directas **solo Android** `jni = 0.21.1` y `ndk-context = 0.1.1`, ya presentes transitivamente por Dioxus. Alternativa explicada al usuario: puente propio Java/Kotlin; se eligió Rust/JNI. Sin versiones nuevas resueltas. |

Se verificaron las APIs contra el SDK API 34 instalado (`javap`), las fuentes de jni 0.21.1/ndk-context 0.1.1/Tao 0.34.8 y la documentación oficial de Android de `KeyGenParameterSpec` (ejemplo HMAC). No se escribió una segunda aplicación en Kotlin.

### Validación realizada

- `cargo check -p discordia --lib --no-default-features --features mobile --target aarch64-linux-android`: exit 0. Advertencias de código desktop aún sin aislar en Android; no se suprimieron.
- `dx build --android --target aarch64-linux-android --no-default-features --features mobile --bin Discordia`: APK generado; instalación y arranque en Moto g15 aprobados.
- El usuario confirmó **«Pude entrar»** tras crear/importar una identidad con la nueva versión.
- Comprobación del JSON de identidad en el dispositivo: `ncryptsec1…`, sin `nsec` ni `seed_phrase` en texto. Marcador `android-keystore-hmac-v1`; archivo `vault.key` ausente. No se imprimieron secretos.
- Se cerró y abrió el proceso Android. Misma identidad activa y pantalla Home visibles después del reinicio: confirma descifrado y carga con Keystore persistente. Se observaron cuatro relays Nostr conectados.
- Evidencia local: `target/mobile-tools/android-persistence-check.json` y captura `C:/Users/Recko/.codex/visualizations/2026/10/20/01a157cb-7e6d-7810-89b7-2128accd1321/mobile-keystore-home.png`. Accesibilidad no entregó jerarquía utilizable; Home se verificó visualmente.
- `cargo test -p discordia --lib keyvault::tests`: **3 passed**, 0 failed.
- `cargo check --workspace`: exit 0, 11.56 s; solo warning del bundle SFU.
- Logs: `target/mobile-tools/{android-keystore-check,dx-keystore-build,keystore-desktop-check,keystore-desktop-tests}.log`.

### Prueba de red y chat

- La UI muestra directorio inaccesible. GET y HEAD a `https://rendezvous.discordia.world/discover` devuelven **HTTP 404 también desde Windows**. No atribuir esa respuesta exclusivamente a Android ni modificar el despliegue remoto sin investigar.
- Se revisó Iroh 1.3: `CaTlsConfig::default()` usa `EmbeddedWebPki`; no se añadió inicialización JNI de rustls-platform-verifier basándose solo en su presencia transitiva.
- El usuario dejó elegir servidor: CLI compilado y servidor **local de prueba** iniciado con `--addr 127.0.0.1:19000 --quic-port 0 --data-dir target/mobile-tools/test-server-data --livekit-autospawn false`. Datos y logs aislados; no se publica en rendezvous. Proceso registrado en `target/mobile-tools/test-server-process.json`; log `test-server.log`. Esta prueba cubre chat, sin SFU para voz.
- `adb reverse tcp:19000 tcp:19000` activo. El teléfono consultó `/discover` al servidor local y recibió 404: prueba que USB llega al servidor, pero la dirección se agregó por error dos veces a **Rendezvous servers**, no al campo de conexión directa. No hubo petición `/gateway` en esa comprobación.
- La conexión directa está debajo del globo: **Other ways to connect → Server address**, base `ws://127.0.0.1:19000` y botón **Connect**. El cliente añade `/gateway`. La ubicación poco visible y la confusión con el directorio quedan pendientes de mejorar en Fase 2. Se pidió al usuario repetir por ese campo; conexión autenticada/chat todavía sin validar. La escritura automatizada no dejó una URL fiable; no considerar un error de URL de esa prueba como fallo de transporte.
- Actualización: usuario confirmó **«entro»**. Log del servidor a las 21:13:32 UTC: `/gateway` respondió **101**, seguido de **identified** con la identidad Android. Consulta SQLite de solo lectura: 1 miembro, 1 guild, 2 canales, 0 mensajes en ese momento. Captura `mobile-chat-connected.png` en la carpeta de evidencias confirma `SERVER: LOCAL`, `#general`, compositor y navegación móvil.
- Defectos visuales confirmados en esa captura: cabecera desborda a la derecha y botón **Activities** se superpone a la navegación inferior. Pendientes de Fase 2; no marcar UI móvil completa.
- Usuario confirmó **«Se envía y veo el campo»**. Nueva consulta SQLite de solo lectura encontró **1 mensaje persistido** (antes 0). El contenido no coincidió literalmente con `prueba Android`; no se imprimió ni se asumió ese texto exacto. Envío de chat y visibilidad del compositor con teclado aprobados para esta prueba manual en Moto g15. Recepción desde un segundo cliente todavía no probada.
- Pendientes dentro de Android: recepción entre dos clientes, permisos y captura de micrófono, audio/auriculares/interrupciones, vuelta de segundo plano, UI táctil completa, clipboard/archivos/enlaces y aislamiento de funciones desktop. No considerar Android completo por guardar la cuenta y enviar un mensaje.

## 9. Android — superposiciones y actividades (2026-10-22)

El usuario reportó que dados y piedra/papel/tijera no abren, y pidió capturar las superposiciones. Evidencia previa: `mobile-overlap-before.png` en la carpeta de capturas de esta sesión.

| Archivo | Cambio / motivo |
|---|---|
| `client/src/features/workspace.rs`, `client/src/app.rs` | Cabecera compacta solo en layout móvil: logo sin palabra, desconectar con icono y etiqueta accesible, controles táctiles de 44px y salto de línea. Actividades pasa a un cuarto botón en la navegación inferior. |
| `client/src/features/activities.rs` | Un único picker compartido con la navegación; quitar el lanzador flotante y tarjetas flotantes en layout móvil. Diálogo acotado al viewport con scroll. |
| `client/src/features/activities.rs`, `client/locales/messages.json` | Dados y RPS comparten el host `activity_window`, deshabilitado en Android/iOS. Explicación visible y botones deshabilitados solo en esos targets; desktop estrecho conserva sus juegos. No se habilitó ejecución de actividades dentro del WebView principal. |
| `client/src/features/activities.rs` | Dejar de descartar `FromActivity::Failed`: reabrir picker con el error y salir de la instancia compartida. Regresión que comprueba reapertura y `ActivityLeave`. |

- No se añadieron dependencias ni APIs Dioxus nuevas.
- Comprobación Windows final: `cargo check --workspace` exit 0 (8.37 s), solo warning de bundle SFU; `cargo fmt --all -- --check` y `git diff --check` aprobados.
- Pruebas de actividades: 9 aprobadas, incluida la regresión de apertura fallida. Check Android: exit 0, 37 warnings del cliente ya presentes.
- `dx build --android --target aarch64-linux-android --no-default-features --features mobile --bin Discordia`: éxito en 95.73 s; `adb install -r`: Success. Inicio en frío: 1706 ms. Gradle detenido al terminar.
- Reconexión desde **Last session → Reconnect** autenticada en el servidor. Mensaje previo visible tras reinstalar. Captura `mobile-layout-after.png`: cabecera completa y cuatro botones inferiores sin superposición. `mobile-activities-status.png`: aviso visible y ambos juegos deshabilitados; `mobile-channels-after.png` y `mobile-members-after.png`: navegación accesible y sin el lanzador flotante. Revisado en vertical en Moto g15; rotación y teclado después de esta actualización todavía no comprobados.
- La pestaña Canales mostró DM seleccionado mientras Chat mostraba `#general`; revisar selección inicial compartida antes de dar por cerrada Fase 2. No se modificó esa selección en esta entrega.
- **Pendiente funcional:** portar un host aislado de actividades a móvil para que dados/RPS funcionen. Esta corrección comunica la limitación; no implementa los juegos en Android. Recepción entre dos clientes, voz, ciclo de vida y resto de pendientes de §8 siguen abiertos.

## 10. Servidor de prueba directo (2026-10-22)

- Usuario pidió servidor y rendezvous para PC/Android/otra red; no dispone de VPS y autorizó omitir rendezvous y usar conexión directa. No se desplegó ni se modificó el VPS documentado en OPS.
- Se reemplazó exclusivamente el proceso local de prueba por el mismo CLI con WS loopback `19000`, QUIC UDP `19001`, dirección pública detectada por STUN y confirmada por UPnP. Misma carpeta `target/mobile-tools/test-server-data`: conserva mensajes y clave QUIC. Metadatos actuales en `direct-server-process.json`; log `direct-server.log`.
- SFU LiveKit 1.13.9 iniciado y validado mediante ListRooms autenticado; base `19880`, ICE TCP `19881` y UDP `19882`. El servidor anterior tenía SFU desactivado. Este cambio no corrige el fallo de WebRTC del APK.
- UPnP aceptó reglas de una hora. Intentar publicar también UDP `19882` produjo respuestas 718 y desapareció la regla QUIC; se retiró solo la regla UDP de voz propia y se priorizaron **UDP 19001 + TCP 19881**, verificadas por GetSpecificPortMappingEntry. No asumir voz UDP externa disponible. `direct-port-mappings.json` registra el estado efectivo.
- La revisión automática rechazó inicialmente elevar la creación de reglas persistentes del firewall. El usuario autorizó explícitamente las cuatro reglas exactas; luego se aplicaron con éxito mediante UAC. Nombres `Discordia-MobileTest-{QUIC19001,VoiceTCP19881,VoiceUDP19882,VoiceLAN19880}`; ejecutables limitados al servidor y SFU de esta prueba. Señalización LAN solo LocalSubnet; las otras tres admiten clientes remotos. No se desactivó el firewall ni se alteraron reglas ajenas.
- Herramientas aisladas bajo `target/mobile-tools`: `start-direct-server.py`, `direct-upnp.py --map/--cleanup`, `allow-direct-server.ps1`, `remove-direct-firewall.ps1`. No se añadió un servicio de arranque ni renovación automática. `--map` conserva las reglas propias vigentes; no prolonga su lease al consultarlas. La PC debe permanecer encendida y la IP pública puede cambiar.
- Datos de conexión y limpieza entregados en `conexion-servidor-prueba.txt` dentro de la carpeta de artefactos de esta sesión. Regla del router temporal; reglas de Windows permanecen hasta ejecutar la retirada como administrador.
- Usuario confirmó **«Entró desde la otra red»**. Log comprobó QUIC autenticado de otro cliente; posteriormente Android muestra mensajes de las otras identidades y la lista de miembros. La IP origen de la primera conexión observada coincidió con la WAN del host: la ubicación fuera de LAN proviene de la confirmación del usuario, no de una prueba geográfica independiente.

## 11. Auditoría visual y fallo de voz Android (2026-10-22)

Revisión física autorizada en Moto g15: 1080×2400, densidad 400. Se capturaron chat vertical, Ajustes, emojis, teclado, miembros y orientación horizontal. La rotación se restauró a sus valores originales (`accelerometer_rotation=0`, `user_rotation=0`). No se cambió código de la app en esta auditoría.

| Prioridad | Hallazgo comprobado | Cambio necesario / archivo principal |
|---|---|---|
| P0 | Voz cierra el proceso con SIGSEGV/null en `CreateAndroidVideoEncoderFactory`. El APK carece de `livekit/org/webrtc/ContextUtils` y `DefaultVideoEncoderFactory`; el build de webrtc-sys registró `Failed to copy libwebrtc.jar`. El arranque Android tampoco llama `init_android_context` ya existente. | Empaquetar las clases de la misma versión WebRTC, inicializar JVM/Context antes de crear el motor y validar entrada/salida de voz. `android.rs`, empaquetado Android y `vendor/webrtc-sys`. No afirmar que agregar solo el permiso corrige el crash. |
| P0 | APK solo declara INTERNET: faltan RECORD_AUDIO y MODIFY_AUDIO_SETTINGS; log del WebView advierte que no tendrá captura. | Manifest real de dx 0.7.10 y petición de permiso en ejecución; manejar rechazo sin cerrar la app. Después probar micrófono, altavoz, auriculares/Bluetooth y pérdida de foco. |
| P1 | Ajustes mantiene menú lateral de escritorio: etiquetas en varias líneas, panel de voz estrecho, scroll horizontal en navegación y scroll vertical anidado. | Navegación superior o drawer para ancho móvil, contenido de una columna y un scroll principal. `features/settings_dialog.rs`, reglas `.dxf-settings*` de `app.rs`. |
| P1 | Al girar a horizontal el breakpoint `max-width:768px` desactiva el modo móvil: desaparece la barra inferior y vuelve el botón flotante sobre el rail. | Mantener shell móvil por plataforma Android/iOS, independientemente de ese breakpoint; desktop puede seguir respondiendo al ancho. `features/workspace.rs`. |
| P1 | Emojis y reacciones usan botones `w-6 h-6` (24px) o `w-7 h-7` (28px), menores a 44px. | Ampliar hitboxes y ajustar columnas/ancho del selector; revisar igualmente menús de mensaje y herramientas de chat. `features/chat.rs`, `chat_tools.rs`. |
| P1 | El teclado mantiene el compositor visible; siguen visibles ayudas Ctrl/Shift/Enter y la navegación inferior, que consumen altura útil. | Ocultar ayudas de teclado de escritorio en móvil; evaluar navegación durante escritura sin tapar el campo. No clasificar el teclado como roto en esta prueba. |
| P1 | Conexión directa escondida debajo del globo y directorios; provocó entradas incorrectas al rendezvous. | Dar acceso visible a conexión por dirección y distinguirlo de configurar directorios. `features/connect.rs`. |
| P1 | Botones de clipboard y diálogos móviles aún devuelven no disponible; ajustes muestran opciones desktop como actualizaciones, detectar juegos y compartir pantalla. | Adaptadores nativos y ocultar/deshabilitar opciones sin soporte; conservar UI/lógica compartidas. |
| P2 | Juegos siguen explícitamente no disponibles; idioma del dispositivo `es-US` pero la cuenta muestra inglés. | Host aislado de actividades; revisar idioma automático Android sin sobrescribir una elección de idioma explícita. |
| Pendiente | Segundo plano, pantalla bloqueada, cambio Wi-Fi/datos, rotación con teclado y voz, notificaciones, batería y distribución release/CI. | Probar y cerrar Fases 3–4. iOS corresponde al colaborador con macOS. |

Evidencias en la carpeta de artefactos: `mobile-audit-{current,settings,emoji,keyboard,landscape,reopened}.png`. La captura solicitada de perfil no abrió una tarjeta; no se considera validada esa tarjeta. Chat/Canales/Miembros y aviso de actividades ya tienen las capturas de §9.

- El usuario avisó después que cerró manualmente y volvió a abrir la app: **no contarlo como otro crash**. Se comprobó nuevo proceso, misma identidad autenticada y mensajes recibidos de otros clientes (`mobile-audit-reopened.png`). Esto valida reapertura y lectura de historial, no todo el ciclo de vida en segundo plano.
- Registros del fallo anterior: `target/mobile-tools/android-voice-crash.log`; SIGSEGV a las 18:28:38 hora local. El reporte de cierre nativo y la ausencia de clases/permisos son evidencia; no se implementó todavía la reparación.
- Orden propuesto para completar Android: **voz/integración nativa → Ajustes/rotación/hitboxes → ciclo de vida y adaptadores → CI/release**. Esta auditoría no declara Android listo para uso diario.
- Verificación al cerrar auditoría: `cargo check --workspace` exit 0, 13.08 s, solo warning de bundle SFU; `git diff --check` aprobado.

## 12. Ejecución completa autorizada (2026-10-22)

El usuario indicó **«hace todo lo que nombraste»**. Continuar por etapas con comprobación Android física y `cargo check --workspace` al cerrar cada una; comunicar el resultado antes de avanzar. iOS sigue a cargo del colaborador con macOS. Mantener una sola lógica/UI compartida y puentes de plataforma acotados.

| Orden | Trabajo | Estado |
|---|---|---|
| A | Empaquetar Java WebRTC de la misma dependencia, inicializar JVM/Context, permisos de micrófono; probar entrada/salida de voz sin crash | Voz base probada; clave del canal antes de publicar y permiso de cámara implementados; escucha entre equipos, cámara física y rechazo de permiso pendientes |
| B | Ajustes de una columna, shell móvil también horizontal, hitboxes de 44px, teclado y navegación; acceso claro a conexión directa | Base implementada e instalada; pruebas físicas aprobadas, menús secundarios pendientes |
| C | Archivos, clipboard y ocultar funciones desktop sin soporte; idioma automático respetando elección | Implementado; texto/idioma/Ajustes y cancelación probados; importación/exportación e imagen de clipboard pendientes |
| D | Juegos incorporados en móvil conservando aislamiento para código externo | Pendiente |
| E | Segundo plano/bloqueo, reconexión/cambio de red, auriculares/Bluetooth/foco de audio, notificaciones | Pendiente |
| F | CI Android, build release reproducible y documentación; revisar peso y límites restantes | Pendiente |

### A — cambios en validación

- `client/android/MainActivity.kt`: mismo WryActivity de Dioxus, puente mínimo de permiso RECORD_AUDIO y resultado de autorización. La aplicación/UI siguen en Rust/Dioxus.
- `client/Dioxus.toml`: `application.android_main_activity` verificado en fuente dx 0.7.10; permisos Android RECORD_AUDIO y MODIFY_AUDIO_SETTINGS mediante su esquema real.
- `scripts/build-android.py`, `client/android/webrtc.init.gradle`: dx genera el proyecto; después se incluye el **libwebrtc.jar de los outputs de enlace del mismo webrtc-sys compilado**, mediante init script de Gradle. Se rechazan artefactos ambiguos/incompletos y se comprueban las clases en los DEX del APK final. La CLI solo acepta coordenadas Maven en `gradle_dependencies`, por eso no se inventó una API para añadir un jar local. Sin dependencias Rust/Maven nuevas; se reutiliza el SDK existente, sin modificar la CLI instalada.
- `android.rs`: inicialización WebRTC en el hilo JNI de Wry (`wry::prelude::dispatch` 0.53.5), validando primero clases y usando ApplicationContext. OnceCell serializa; se pide permiso al utilizar voz, no al iniciar sesión. Denegación/timeout devuelven error y no abren captura.
- `features/voice.rs`, `voice_mic_test.rs`, `dm_call.rs`: preparación previa al motor/llamada/prueba y comprobación de permiso antes de abrir MicCapture. Suscriptores de audio de pantalla inicializan WebRTC sin pedir permiso de micrófono.
- Check Android: exit 0 (37 warnings ya presentes). Check workspace Windows: exit 0, 15.92 s, solo warning bundle SFU. APK generado y validación de clases DEX aprobada. Instalación y prueba física en curso; regresiones DM ejecutándose.

### A — fallos físicos posteriores y enlace JNI

- Primer APK con clases: `NoClassDefFoundError` por loader de arranque en el looper JNI. Se agregó `MainActivity.initializeWebRtc`, llamada desde Rust en el hilo UI: aporta el frame Java de la aplicación antes de `init_android_context`. El usuario vio el permiso de micrófono tras instalar esta corrección.
- Después del permiso: `UnsatisfiedLinkError` en `SoftwareVideoEncoderFactory.nativeCreateFactory`; CheckJNI aborta por excepción pendiente. Reapertura repite el cierre. `llvm-nm -D` confirmó que faltaba el método JNI en `libmain.so`. No borrar datos ni reinstalar desde cero: preservar identidad/Keystore.
- `vendor/webrtc-sys/Cargo.toml`, `vendor/webrtc-sys-build/src/lib.rs`, `client/build.rs`: propagar la lista JNI mediante metadata Cargo (`links`) y retener/exportar esos símbolos en el enlace final Android. Los argumentos del build script de una dependencia no llegan al ejecutable. Sin nuevas dependencias ni versiones.
- `scripts/build-android.py`: comprobar exportaciones JNI de encoder, decoder y puente propio dentro del APK final con `llvm-nm` del NDK; rechazar antes de instalar si faltan. Compilación/validación física de esta corrección en curso.
- Regresiones DM previas: 13 aprobadas; logs `android-voice-dm-regression.log`. Nuevo registro físico: `android-voice-permission-crash.log`. USB autorizado nuevamente por el usuario. Servidor de prueba reiniciado conservando datos; reverse USB 19000 y 19880 restaurados.
- APK JNI generado y validado: encoder/decoder y puente propio exportados; clases DEX presentes. `adb install -r` Success, identidad conservada. Check workspace exit 0 (1m37s; recompilación vendor), solo warning bundle SFU. Log `android-voice-jni-build.log`.
- Prueba física: proceso 10565 sobrevivió a entrada/salida en **Lobby → General Voice** vacío. `Connected`, Opus y ruta IPv6/UDP; captura y playback 48 kHz estéreo abiertos, callbacks de micrófono continuos. No hay otro participante: esto no valida escucha bidireccional ni calidad. Captura `mobile-voice-jni-join.png`; log `android-voice-jni-join.log`.
- Nuevo defecto confirmado: cada escaneo de dispositivos (5 s) reabre captura/salida. CPAL AAudio enumera dispositivos físicos pero `default_input/output_device` devuelve una ruta virtual `Default Device` ausente de esa lista. `audio_device::missing` y `voice::maintain` conservan esa ruta solo en Android; desktop conserva detección anterior. Regresión cubre ruta predeterminada, dispositivo físico y auricular desconectado. Build/prueba física pendientes.

### B — cambios en validación

| Archivos | Motivo |
|---|---|
| `client/assets/mobile.css`, `src/app.rs` | Reglas acotadas a clase de plataforma Android/iOS: no aplicar a desktop por ancho. Viewport dinámico, safe areas en Ajustes, navegación horizontal y contenido de una columna, controles de 44px. |
| `src/features/settings_dialog.rs` | Marcadores CSS y etiqueta accesible del cierre; mismas vistas y estado. |
| `src/features/workspace.rs` | No sobrescribir el modo móvil nativo con el breakpoint al rotar. |
| `src/features/chat.rs` | Selectores emoji de seis columnas con hitboxes de 44px y scroll acotado; ocultar ayudas de teclado desktop conservando estado de carga de archivos. |
| `src/features/connect.rs`, `locales/messages.json` | Conexión por dirección antes del globo, etiqueta explícita traducida; misma lógica y campo vacío inicial. |

Sin dependencias nuevas. Check workspace previo al último ajuste de audio: exit 0, 26.98 s. Verificación física de B pendiente; no afirmar tamaños/rotación corregidos sin nuevas capturas.

### A/B — segunda instalación y medidas físicas

- `cargo test -p discordia --lib audio_device::tests`: **5 aprobadas**, incluido default virtual Android. `cargo check --workspace`: exit 0, 33.48 s, solo warning bundle SFU. Build Android completo y validación DEX/JNI aprobados; `adb install -r` Success. Logs `android-default-route-tests.log`, `mobile-audio-layout-{build,desktop-check}.log`.
- Inspector de WebView de depuración por USB, sin modificar mensajes: viewport 432×895 CSS px, DPR 2.5. Ajustes pasó de contenido **266.4 px** y sidebar 116 px a contenido **414.4 px**, navegación horizontal encima. Ningún control medido de Ajustes menor a 44 px de alto. El overflow de botones fuera de vista corresponde al carrusel de navegación con scroll, no al contenido.
- Selector del compositor: **40 botones de 44×44 px**, seis columnas, ningún desborde horizontal. Capturas `mobile-settings-tabs-after.png`, `mobile-emoji-touch-after.png`, `mobile-chat-no-hints-after.png`.
- Horizontal: mantiene una sola columna y las cuatro pestañas inferiores; no reaparece el lanzador flotante. `mobile-landscape-shell-after.png`; se restauró `user_rotation=0`, `accelerometer_rotation=0`.
- Teclado: nueva prueba confirma campo oculto; ni layout viewport ni visualViewport se reducen (ambos 895 px). Manifest generado no especifica softInputMode. `client/android/MainActivity.kt::onCreate` agrega `SOFT_INPUT_ADJUST_RESIZE` explícito. Build del puente y exportaciones JNI aprobados (`mobile-keyboard-native-build.log`); instalación y comprobación pendientes. No marcar teclado aprobado con la captura actual.
- Medidas JSON y helper local reproducible bajo `target/mobile-tools/mobile-*-metrics.json`, `android-dom-metrics.mjs`. Capturas en la carpeta de artefactos de esta sesión. El inspector solo se usa en build de depuración por USB.

### Cierre del punto de control A/B

- Segundo ingreso a General Voice con el default corregido: `Connected` y captura continua por más de 25 s, sin `microphone reopened` ni `output reopened`, atravesando varios escaneos de 5 s. Se salió de la llamada; mismo proceso vivo. Evidencia `android-default-route-device.log`. Esto corrige reinicios periódicos, no demuestra ausencia de toda oscilación ni calidad entre equipos.
- Puente de teclado instalado con `adb install -r`: Success. `SOFT_INPUT_ADJUST_RESIZE` reduce layout y visualViewport de **895 a 549 px**; compositor visible sobre el teclado, sin desborde horizontal. Captura `mobile-keyboard-resize-after.png`, medidas `mobile-keyboard-resize-metrics.json`. No se escribió ni envió un mensaje en esta prueba.
- APK actual incluye el mismo Rust validado por build completo más `MainActivity.onCreate` recompilado con Gradle. La fuente original está en `client/android/MainActivity.kt`; el próximo build completo de `scripts/build-android.py` vuelve a copiarla de forma reproducible. No depender de una edición solo en `target/`.
- **Próximo trabajo:** C (clipboard/archivos/idioma/aislar opciones desktop); D (host móvil aislado de juegos); E (audio entre dos equipos, denegación de permisos, foco/Bluetooth, segundo plano/red); F (CI, release y guía). Revisar también cabecera de voz extensa y menús de reacciones/mensajes: no se validaron todos los controles de la app. No marcar Android completo.
- Rama `mobile-platform-support`, cambios locales sin publicar. No borrar identidad, datos de prueba ni trabajo previo. El APK debug conserva símbolos y pesa aproximadamente 727 MB: evaluar tamaño y distribución en F; no tomarlo como tamaño release.
- Check final A/B `cargo check --workspace`: exit 0, **1.05 s**, solo warning bundle SFU; `cargo fmt --all -- --check` y `git diff --check` aprobados. Log `mobile-ab-final-workspace-check.log`. App abierta sin llamada; inspector USB 19222 retirado, reverse del servidor conservado.

### C — adaptadores Android y marca (2026-10-22)

| Cambio | Implementación / evidencia |
|---|---|
| Clipboard | `MainActivity.kt` + `android.rs`: copiar/leer texto y leer imágenes `content://` hasta 2 MiB; imágenes pasan por el validador compartido. Round trip de texto ficticio aprobado mediante el menú real y JNI. No se leyó contenido previo del portapapeles. Imagen pendiente de prueba física. |
| Documentos | `ACTION_OPEN_DOCUMENT` / `ACTION_CREATE_DOCUMENT`, sin permiso general de almacenamiento. Copia privada hasta 32 MiB; nombre saneado y `PickedFile` elimina el temporal al terminar. Guardado por ContentResolver, sin ejecutar el archivo. Solicitudes serializadas, cancelación al abandonar la vista y resultados identificados para descartar callbacks viejos. |
| Prueba de documentos | Apertura y cancelación aprobadas, mismo proceso 20985 vivo. Se copió solo `Download/mobile-c-fixture.txt` como fixture; no se seleccionaron archivos personales ni se enviaron mensajes. Importación, rechazo de tamaño y exportación siguen sin validación física. |
| Idioma | Configuration/Locale de Android, conservando `Language::System` y toda elección explícita. Dispositivo `es-US`, selector `system`, documento `es`. Catálogo completa Back to chat/Channels/Chat. |
| Opciones desktop | Ajustes oculta Video/Updates y detección/RPC de juegos en móvil; conserva nivel global. Conexión oculta crear servidor. Las cuatro pestañas móviles de Ajustes fueron comprobadas en el dispositivo. |
| Ícono y arranque | Assets existentes: ícono adaptativo, PNG mdpi–xxxhdpi, splash nativo 26+/31+ y SVG inline en el documento de carga compartido. Sin espera artificial. Ficha de Android muestra Discordia, captura `target/mobile-tools/mobile-branding-app-info.png`. |
| Empaquetado | `scripts/build-android.py` aplica recursos después de dx y prepara los recursos persistentes antes de repetirlo: dx regenera sus templates pero no limpia extras. Reemplaza únicamente sus cinco WebP de launcher. Sigue validando WebRTC/JNI y ahora verifica recursos de marca. |
| Checks | Workspace aprobado (14.67 s; warnings de caché incremental y bundle SFU), 7 pruebas i18n, 1 documento de arranque, 3 JS de arranque, 4 catálogo/traducciones. Build Android completo y comprobaciones DEX/JNI/marca aprobados; log `mobile-c-final-build.log`. |

- Rama conservada: `mobile-platform-support`; sin commits ni publicación. No borrar identidad/Keystore ni datos del servidor. APK debug actual ~368 MB comprimido; no es una medición release.
- Instalación final con `adb install -r` comprobada por `lastUpdateTime=2026-10-22 23:15:47`; proceso 26346 abrió sin fallo nativo observado. Arranque en frío muestra el logo propio (`mobile-branding-cold-start.png`); identidad conservada. Inspector 19222 retirado, reverse del servidor conservado.
- Sigue pendiente cerrar pruebas de C, D–F y los controles secundarios de B. El cambio de marca no valida notificaciones, cámara, voz bidireccional ni ciclo de vida.

### A — clave del canal antes de publicar, y permiso de cámara Android (2026-10-22)

| Cambio | Implementación / evidencia |
|---|---|
| Helper compartido | `client/src/e2ee.rs::ensure_channel_key`: la espera que ya usaba la voz (10 s, mensaje "The voice encryption key did not arrive. Leave and rejoin the channel.") pasa a un helper; devuelve error si la persona sale del canal mientras espera y aplica la clave ganadora con `apply_key`. |
| Voz y salas nativas | `client/src/features/voice.rs`: `ActiveVoice::connect` usa el helper; `set_screen_video` y `set_camera` (sala nativa compartida de pantalla/cámara, Windows/macOS) esperan la clave **antes** de `video_room`, porque `register_room` desactiva E2EE sin clave y `room_options` entregaría la clave provisional del proveedor. |
| Sala WebView | `client/src/features/screenshare.rs` (`SCREEN_JS`): la sala ya no se une con `setE2EEEnabled(false)` cuando falta la clave. `awaitE2eeKey` espera hasta 10 s, `setE2eeKey` despierta a quien espera y, sin clave, cierra la sala, publica `e2ee-error` y reporta `retry` para reintentar con la clave. `startCamera` y el inicio de pantalla (`startShare` y la ruta `getDisplayMedia`) esperan lo mismo antes de crear/publicar la pista. |
| Permiso de cámara | `client/Dioxus.toml` declara `android.permission.CAMERA`; `client/android/MainActivity.kt` añade `requestCameraPermission`/`cameraPermissionResult` (código 602); `client/src/android.rs` añade `permission_granted`, `check_camera_permission`, `camera_permission_granted` y `prepare_camera` (comparte `PERMISSION_LOCK` con el micrófono); `features/camera.rs` pide el permiso antes de arrancar la cámara del WebView y muestra el error si se deniega. El `RustWebChromeClient` que genera dx ya pedía `RESOURCE_VIDEO_CAPTURE`: sin el permiso en el manifest esa petición se denegaba sola, que era el bloqueo real. |
| Pruebas | `client/tests/video_lifecycle_bridge.cjs` (incluido por `screenshare_bridge.cjs`, ejecutado en CI) suma dos casos: sin clave la sala no se une ni enciende E2EE y `setE2eeKey` la une cifrada; y la clave ausente se reporta como `retry` sin unirse ni publicar. `node` no está instalado en la máquina de desarrollo: estos casos se ejecutan en CI. |

- Checks: `cargo check --workspace` exit 0 (solo warning del bundle SFU); check Android exit 0 con **37** warnings del cliente (sin nuevos); `cargo fmt --all -- --check` y `git diff --check` aprobados.
- Pendiente físico: build del APK por `scripts/build-android.py`, `adb install -r` y prueba de cámara (preview + publicación en llamada) y de voz entre dos equipos. Esta entrega no la sustituye. Sin dependencias nuevas.
- Rama `mobile-platform-support`, cambios locales sin publicar; no se borró identidad, Keystore ni datos del servidor de prueba.

### Revisión y rendimiento (2026-10-23)

- Base actual `5b853c4`; se conservaron los avances de cámara/pantalla nativas. Errores, alcance y mediciones reproducibles en [ANDROID_REVIEW.md](ANDROID_REVIEW.md).
- Corregidos: orden consentimiento/servicio de MediaProjection, referencia I420, doble parada/callbacks de capturas viejas, limpieza al cancelar y bloqueo del empaquetador por pipes de Gradle.
- APK corregido compilado, validado e instalado con `adb install -r`. En Moto g15/API 35, Inicio debug pasó de CPU mediana 153.3% a 10.0% (18.7% después de repetir Ajustes), eliminando animación decorativa continua móvil de logo/globo. 100% = un núcleo; no incluye renderer aislado.
- Se midieron Inicio y diez ciclos de Ajustes, repetidos. No equivale a rendimiento release ni valida voz/cámara/pantalla después de estas correcciones; fases A/E/F siguen abiertas. Sin commits ni publicación de esta revisión.
- Chat/Canales retenidos: medianas de 837→121 ms y 401→116 ms en el mismo canal caliente. Cabecera de una fila, Guardados solo en Chat dentro del workspace, hora/batería visibles por pedido del usuario.
- Carga directa de identidad activa con comprobación criptográfica y migración sin descifrados redundantes: arranque debug de proceso frío 4.46→2.8–3.3 s (3 muestras/lote); reapertura sin mejora demostrada. Detalles y límites en el informe de revisión.
- Segundo plano: servicio común `microphone|camera|mediaProjection`, tipos según uso y wake lock acotado al servicio; voz/cámara esperan confirmación, detener un medio conserva los demás. APK instalado 10:40:18; usuario confirma voz audible y recepción de cámara/pantalla desde la PC al cambiar de app. ADB verifica servicio/wake lock/mismo PID durante ~2 min, incluida pantalla en Dozing en voz. Llamadas largas y video con pantalla bloqueada sin validar; no garantiza supervivencia frente a cierre forzado/fabricante ni reinicia capturas sin consentimiento.
- Visor móvil: espacio 16:9 desde conexión, controles táctiles de 44 px y volumen directo al mezclador. Preview de cámara Android local y nativa, con rotación, cola de un frame y liberación antes de publicar; usuario confirma imagen y mejor respuesta de audio. Muestreo 1440p/60 muestra presión intermitente real; comparación 1080p/30 y límites en el informe de revisión.
