from pathlib import Path
import os
import shutil
import subprocess

root = Path(__file__).resolve().parents[1]
output = root / "target/mobile-tools/network-callback-test"
output.mkdir(parents=True, exist_ok=True)
cache = Path.home() / ".gradle/caches/modules-2/files-2.1"


def jar(group, name, version=None):
    base = cache / group / name
    return str(sorted((base / version if version else base).rglob(name + "-*.jar"))[-1])


stdlib = jar("org.jetbrains.kotlin", "kotlin-stdlib", "2.0.20")
annotations = jar("org.jetbrains", "annotations")
compiler = os.pathsep.join([
    jar("org.jetbrains.kotlin", "kotlin-compiler-embeddable", "2.0.20"), stdlib,
    jar("org.jetbrains.kotlin", "kotlin-script-runtime", "2.0.20"),
    jar("org.jetbrains.kotlin", "kotlin-reflect"), jar("org.jetbrains.intellij.deps", "trove4j"),
    jar("org.jetbrains.kotlinx", "kotlinx-coroutines-core-jvm"), annotations,
])
java = str(Path(os.environ["JAVA_HOME"]) / "bin" / ("java.exe" if os.name == "nt" else "java")) if os.environ.get("JAVA_HOME") else shutil.which("java")
if not java:
    raise RuntimeError("Java is required; run the Android build first")
source = (root / "client/android/MainActivity.kt").read_text(encoding="utf-8")
start = source.index("    private fun watchDefaultNetwork()")
opening = source.index("{", start)
depth, end = 1, opening + 1
while depth:
    depth += (source[end] == "{") - (source[end] == "}")
    end += 1
method = source[start:end].replace("private fun", "fun").replace("android.util.Log", "Log")
fixture = '''
class Network
class LinkProperties
object Context { const val CONNECTIVITY_SERVICE = "connectivity" }
object Log { fun i(tag: String, text: String) {}; fun w(tag: String, text: String, error: Exception) {} }
class ConnectivityManager {
    open class NetworkCallback {
        open fun onAvailable(network: Network) {}
        open fun onLost(network: Network) {}
        open fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) {}
        open fun onBlockedStatusChanged(network: Network, blocked: Boolean) {}
    }
    lateinit var registered: NetworkCallback
    fun registerDefaultNetworkCallback(callback: NetworkCallback) { registered = callback }
}
class Harness {
    val manager = ConnectivityManager()
    var networkCallback: ConnectivityManager.NetworkCallback? = null
    var refreshes = 0
    fun getSystemService(name: String): Any = manager
    fun onNetworkChanged() { refreshes++ }
'''
checks = '''
}
fun main() {
    val app = Harness()
    app.watchDefaultNetwork()
    check(app.networkCallback === app.manager.registered)
    val callback = app.manager.registered
    val network = Network()
    callback.onBlockedStatusChanged(network, true)
    check(app.refreshes == 0) { "Blocked access must not trigger a futile reconnect" }
    callback.onBlockedStatusChanged(network, false)
    check(app.refreshes == 1) { "Unblocking access must reconnect even when the network did not change" }
    callback.onAvailable(network)
    callback.onLinkPropertiesChanged(network, LinkProperties())
    callback.onLost(network)
    check(app.refreshes == 4) { "Existing network change callbacks must still refresh" }
    println("Network callback checks pass")
}
'''
path = output / "NetworkCallback.kt"
path.write_text(fixture + method + checks, encoding="utf-8")
result = subprocess.run([java, "-cp", compiler, "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler", "-no-stdlib", "-no-reflect", "-classpath", os.pathsep.join([stdlib, annotations]), "-d", str(output / "classes"), str(path)], capture_output=True, text=True)
if result.returncode:
    print(result.stderr)
    raise SystemExit(result.returncode)
subprocess.run([java, "-cp", os.pathsep.join([str(output / "classes"), stdlib]), "NetworkCallbackKt"], check=True)
