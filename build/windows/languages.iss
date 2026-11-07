; Languages for the Freemonplay installer.
; Inno Setup picks the one matching Windows' display language (English otherwise). Its own
; messages come with Inno Setup; Simplified Chinese ships with Inno Setup 6.5 and later,
; with a copy in languages\ for older compilers. Our few lines are below, in each language.

[Setup]
ShowLanguageDialog=auto

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
Name: "tr"; MessagesFile: "compiler:Languages\Turkish.isl"
#if FileExists(CompilerPath + "Languages\ChineseSimplified.isl")
Name: "zh"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
#else
Name: "zh"; MessagesFile: "languages\ChineseSimplified.isl"
#endif

[CustomMessages]
en.StartupTask=Start Freemonplay Host when I sign in
de.StartupTask=Freemonplay Host bei der Anmeldung starten
es.StartupTask=Iniciar Freemonplay Host al iniciar sesión
fr.StartupTask=Lancer Freemonplay Host à l’ouverture de session
tr.StartupTask=Oturum açtığımda Freemonplay Host'u başlat
zh.StartupTask=登录时启动 Freemonplay Host
