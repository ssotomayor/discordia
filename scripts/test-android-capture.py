from pathlib import Path
import os,subprocess,shutil
root=Path.cwd();out=root/'target/mobile-tools/capture-geometry-test';out.mkdir(exist_ok=True)
cache=Path.home()/'.gradle/caches/modules-2/files-2.1'
def jar(group,name,version=None):
 b=cache/group/name;return str(sorted((b/version if version else b).rglob(name+'-*.jar'))[-1])
stdlib=jar('org.jetbrains.kotlin','kotlin-stdlib','2.0.20');annotations=jar('org.jetbrains','annotations')
compiler=os.pathsep.join([jar('org.jetbrains.kotlin','kotlin-compiler-embeddable','2.0.20'),stdlib,jar('org.jetbrains.kotlin','kotlin-script-runtime','2.0.20'),jar('org.jetbrains.kotlin','kotlin-reflect'),jar('org.jetbrains.intellij.deps','trove4j'),jar('org.jetbrains.kotlinx','kotlinx-coroutines-core-jvm'),annotations])
java_home=os.environ.get('JAVA_HOME')
java=str(Path(java_home)/'bin'/('java.exe' if os.name=='nt' else 'java')) if java_home else shutil.which('java')
if not java: raise RuntimeError('Java is required; run the Android build first')
def method(source,name):
 start=source.rfind('\n',0,source.index('fun '+name))+1
 opening=source.index('{',start);depth=1;end=opening+1
 while depth:
  depth+=(source[end]=='{')-(source[end]=='}');end+=1
 return source[start:end].replace('android.util.Log.i','testLog')
fixture='''class Configuration
open class Base {open fun onConfigurationChanged(c:Configuration) {}}
class DisplayMetrics(var widthPixels:Int=0,var heightPixels:Int=0)
class Display(var w:Int=1080,var h:Int=2400) {fun getRealMetrics(m:DisplayMetrics) {m.widthPixels=w;m.heightPixels=h};companion object {const val DEFAULT_DISPLAY=0}}
class DisplayManager(val display:Display) {fun getDisplay(id:Int)=display}
object Context { const val DISPLAY_SERVICE="display" }
class Resources {val displayMetrics=DisplayMetrics(2400,1066)}
class Capturer {var fail=false;var calls=0;fun changeCaptureFormat(w:Int,h:Int,fps:Int){calls++;if(fail)error("capture failure")}}
fun testLog(tag:String,message:String) {}
class Harness:Base() {
 val resources=Resources();val physical=Display();fun getSystemService(name:String):Any=DisplayManager(physical)
 var screenSlot=1;var screenResult=1;var screenMaxWidth=640;var screenMaxHeight=360;var screenWidth=640;var screenHeight=284;var screenFps=30
 var capturedContentWidth=0;var capturedContentHeight=0;val screenCapturer:Capturer?=Capturer()
'''
case='''}
fun main() {
 val h=Harness()
 check(h.captureSize(640,360)==Pair(288,640)) {"stale landscape activity changed portrait capture: ${h.captureSize(640,360)}"}
 %s
 println("Capture geometry checks pass")
}
'''
extra='''h.updateCapturedContentSize(1,1080,2400)
 check(h.screenWidth==288&&h.screenHeight==640)
 val calls=h.screenCapturer!!.calls
 check(h.resizeScreenShare(1,640,360,20));check(h.screenCapturer.calls==calls) {"FPS change recreated the display"}
 h.updateCapturedContentSize(1,2400,1080);check(h.screenWidth==640&&h.screenHeight==288&&h.screenFps==20)
 h.updateCapturedContentSize(0,1080,2400);check(h.capturedContentWidth==2400)
 h.updateCapturedContentSize(1,0,0);check(h.capturedContentWidth==2400)
 check(h.resizeScreenShare(1,960,540,30));check(h.screenWidth==960&&h.screenHeight==432)
 h.screenCapturer.fail=true
 h.updateCapturedContentSize(1,1080,2400);check(h.screenWidth==960&&h.screenHeight==432)
 check(h.screenMaxWidth==960&&h.screenMaxHeight==540)
'''
source=(root/'client/android/MainActivity.kt').read_text(encoding='utf-8')
names=['captureDimensions','updateCapturedContentSize','captureSize','resizeScreenShare','onConfigurationChanged']
text=(fixture+'\n'.join(method(source,n) for n in names)+(case%extra)).replace('private fun','fun')
path=out/'CaptureGeometry.kt';path.write_text(text,encoding='utf-8')
result=subprocess.run([java,'-cp',compiler,'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-classpath',os.pathsep.join([stdlib,annotations]),'-d',str(out/'classes'),str(path)],capture_output=True,text=True)
if result.returncode:print(result.stderr);raise SystemExit(result.returncode)
result=subprocess.run([java,'-cp',os.pathsep.join([str(out/'classes'),stdlib]),'CaptureGeometryKt'],capture_output=True,text=True)
print(result.stdout+result.stderr,end='')
raise SystemExit(result.returncode)
