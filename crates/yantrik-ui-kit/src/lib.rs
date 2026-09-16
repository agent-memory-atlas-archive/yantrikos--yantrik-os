// Yantrik UI Kit — reusable Slint components.
// Consuming crates access .slint files via DEP_YANTRIK_UI_KIT_SLINT_PATH env var.
//
// Components: AppHeader, YButton, YIconButton, YInput, YDialog, YTabs,
// YSidebar, YListItem, YContextMenu, YEmptyState, ShellCard, ShellPanel,
// LineChart, RadarChart, ToastBanner, MessageBubble.
//
// AppHeader is the one every app is required to use; see docs/app-sdk.md. YToolbar and
// AppShell used to sit here and were instantiated by nothing, because each spent Slint's
// single `@children` on a region no app needed. They are gone.
