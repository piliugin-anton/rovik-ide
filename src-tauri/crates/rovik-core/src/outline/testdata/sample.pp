class nginx (String $version = 'latest') {
  package { 'nginx': ensure => $version }
}
define nginx::site ($root) {
  file { $root: ensure => directory }
}
node 'web01' { include nginx }
function app::helper(String $x) >> String { $x }
