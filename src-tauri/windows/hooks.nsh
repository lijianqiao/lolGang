; NSIS 安装包的钩子（tauri.conf.json 的 bundle.windows.nsis.installerHooks）。文件要存成带 BOM 的 UTF-8、CRLF 换行，NSIS 才按 UTF-8 读中文。
; 在 Tauri 模板（tauri-cli 2.12.4）里，这个文件在 utils.nsh、Win\COM.nsh、FileFunc.nsh 之后、所有 Section 之前引入：
; 可以用它们的宏（IsShortcutTarget、SetLnkAppUserModelId、GetParent），也可以声明变量。
;
; 一、从 lolGang 迁移（3.3.0：程序改名 lolPing，见 docs/v3.3/requirements.md 2.0 节）
; Tauri 模板按产品名记的东西都换了一套：安装位置（HKCU\Software\lijianqiao\<产品名>）、卸载信息（…\Uninstall\<产品名>）、
; 开始菜单和桌面快捷方式（<产品名>.lnk）、开机启动（HKCU\…\Run 里名叫 <产品名> 的一项），exe 也从 lolgang.exe 换成 lolping.exe。
; 不迁移的话，自动更新装出来的是一个新程序，旧的 lolGang 还留在原处。所以：
; 1. 只认注册表里 lolGang 自己记的安装位置（Software\lijianqiao\lolGang），目录里同时有 lolgang.exe 和 uninstall.exe 才算。
;    不看快捷方式：2.0.1 那版迁移按快捷方式找目录、运行里面的 uninstall.exe，有人放一个伪造的快捷方式就能让安装包运行他的程序（3.2.0 删掉时记过）。
; 2. 自动更新（/UPDATE）时还没有 lolPing 的安装记录，模板会装到默认目录 %LOCALAPPDATA%\lolPing；改成装在旧目录旁边
;    （D:\Program Files\lolGang → D:\Program Files\lolPing）。手动安装时用户自己选目录，不改。
; 3. 不覆盖别人的 lolPing：原版 lolPing（另一个作者的程序）也叫 lolPing.exe。第一次装 lolPing 时目标目录里已经有 lolping.exe，
;    就是别人的：自动更新时换回默认目录，手动安装时停下来让用户换目录。不然会把它的程序覆盖掉（安装包还会先把它关掉）。
; 4. 装好后静默卸掉旧的：旧的卸载程序会关掉正在运行的 lolgang.exe，删它的文件、卸载信息、指向它的快捷方式和开机启动项。
;    静默卸载不勾「删除数据」；数据目录按应用标识 com.lijia.lolgang 存，两者共用，留着。
;    装在同一个目录里的（手动安装时选了旧目录）不能这样做：旧的卸载程序也叫 uninstall.exe，会把新的删掉。只删旧 exe、旧快捷方式和旧的注册表项。
; 5. 自动更新时模板不建快捷方式，旧的又随卸载删了：补上开始菜单的；原来桌面上有 lolGang 快捷方式的，桌面也补。
;    已经有一个叫 lolPing.lnk 的（多半是原版 lolPing 的）不覆盖。
; 6. 开机启动不在这里管：旧的卸载程序删掉 Run\lolGang；新程序启动时看设置里开着开机启动、系统里没登记，就登记 Run\lolPing（api.rs 的 ensure_autostart）。
;
; 二、每次装完（包括自动更新）都告诉 Windows 图标可能变了（3.1.1）：Windows 按路径缓存了 exe 的图标；
; Tauri 的模板装完不通知、自动更新时也不重建快捷方式，换了图标后桌面上会一直是旧图。

!define LOLGANG_KEY "${MANUKEY}\lolGang"
!define LOLGANG_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\lolGang"

; 旧的 lolGang 装在哪（第 1 条）；没有就是空的。PREINSTALL 里找，POSTINSTALL 里用。
Var OldLolGangDir

!macro LOLPING_FIND_OLD
  ReadRegStr $OldLolGangDir SHCTX "${LOLGANG_KEY}" ""
  ${If} $OldLolGangDir != ""
    ${IfNot} ${FileExists} "$OldLolGangDir\lolgang.exe"
    ${OrIfNot} ${FileExists} "$OldLolGangDir\uninstall.exe"
      StrCpy $OldLolGangDir ""
    ${EndIf}
  ${EndIf}
!macroend

; 这时 SetOutPath 已经建了目标目录（还是空的），模板还没写安装记录、没复制文件。
!macro NSIS_HOOK_PREINSTALL
  !insertmacro LOLPING_FIND_OLD
  ReadRegStr $R7 SHCTX "${MANUPRODUCTKEY}" ""
  ; 第 2 条：自动更新、第一次装 lolPing、有旧的 lolGang → 装在旧目录旁边（那里已经有别人的 lolping.exe 就不去，第 3 条）
  ${If} $UpdateMode = 1
  ${AndIf} $OldLolGangDir != ""
  ${AndIf} $R7 == ""
    ${GetParent} $OldLolGangDir $R6
    ${IfNot} ${FileExists} "$R6\${PRODUCTNAME}\${MAINBINARYNAME}.exe"
      RMDir $INSTDIR
      StrCpy $INSTDIR "$R6\${PRODUCTNAME}"
      SetOutPath $INSTDIR
    ${EndIf}
  ${EndIf}
  ; 第 3 条：第一次装 lolPing，目标目录里却已经有 lolping.exe，那是别人的
  ${If} $R7 == ""
  ${AndIf} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
    ${If} $UpdateMode = 1
      RMDir $INSTDIR
      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"
      SetOutPath $INSTDIR
    ${EndIf}
    ${If} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
      MessageBox MB_ICONSTOP "$INSTDIR 里已经有一个 lolPing.exe，多半是原版 lolPing（另一个作者的程序）。为了不覆盖它，请换一个安装目录。"
      Abort
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ${If} $OldLolGangDir != ""
    ; 原来桌面上有没有指向旧程序的快捷方式（卸掉旧的之前看）
    !insertmacro IsShortcutTarget "$DESKTOP\lolGang.lnk" "$OldLolGangDir\lolgang.exe"
    Pop $R5
    ${If} $OldLolGangDir == $INSTDIR
      ; 第 4 条：装在同一个目录，不运行旧的卸载程序
      !insertmacro IsShortcutTarget "$SMPROGRAMS\lolGang.lnk" "$INSTDIR\lolgang.exe"
      Pop $0
      ${If} $0 = 1
        Delete "$SMPROGRAMS\lolGang.lnk"
      ${EndIf}
      ${If} $R5 = 1
        Delete "$DESKTOP\lolGang.lnk"
      ${EndIf}
      Delete "$INSTDIR\lolgang.exe"
      DeleteRegKey SHCTX "${LOLGANG_UNINSTKEY}"
      DeleteRegKey SHCTX "${LOLGANG_KEY}"
      DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "lolGang"
    ${Else}
      ; 第 4 条：_?= 让卸载程序就地运行，并等它结束；它不会删自己，所以之后手动删掉
      ExecWait '"$OldLolGangDir\uninstall.exe" /S _?=$OldLolGangDir'
      Delete "$OldLolGangDir\uninstall.exe"
      RMDir "$OldLolGangDir"
    ${EndIf}
    ; 第 5 条：补快捷方式，已经有同名的不覆盖
    ${IfNot} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}
    ${If} $R5 = 1
    ${AndIfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
      CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}
  ; 二：SHCNE_ASSOCCHANGED（0x08000000）、SHCNF_IDLIST（0）：资源管理器丢掉图标缓存，桌面和开始菜单的快捷方式重新取图标
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend
