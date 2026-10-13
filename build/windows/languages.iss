; The installers' languages, shared by freemonplay-host.iss and freemonplay-screen.iss.
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

en.InstallingDriver=Installing the display driver and the firewall rule...
de.InstallingDriver=Bildschirmtreiber und Firewall-Regel werden installiert...
es.InstallingDriver=Instalando el controlador de pantalla y la regla del firewall...
fr.InstallingDriver=Installation du pilote d’affichage et de la règle du pare-feu...
tr.InstallingDriver=Ekran sürücüsü ve güvenlik duvarı kuralı yükleniyor...
zh.InstallingDriver=正在安装显示驱动程序和防火墙规则...

en.HostComment=Give this PC extra screens
de.HostComment=Diesem PC zusätzliche Bildschirme geben
es.HostComment=Añade pantallas adicionales a este PC
fr.HostComment=Donnez des écrans supplémentaires à ce PC
tr.HostComment=Bu bilgisayara ek ekranlar ekleyin
zh.HostComment=为这台电脑添加扩展屏幕

en.ScreenComment=Use this computer as an extra screen
de.ScreenComment=Diesen Computer als Zusatzbildschirm nutzen
es.ScreenComment=Usa este ordenador como pantalla adicional
fr.ScreenComment=Utilisez cet ordinateur comme écran supplémentaire
tr.ScreenComment=Bu bilgisayarı ek ekran olarak kullanın
zh.ScreenComment=将这台电脑用作扩展屏幕
