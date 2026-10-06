function Get-Thing {
  param($x)
  $x
}
class Widget {
  [string]$Name
  [void] Render() {}
}
enum Mode { On; Off }
