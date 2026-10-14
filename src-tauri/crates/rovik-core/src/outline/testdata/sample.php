<?php
namespace App\Models;
use Foo\Bar;
interface HasName { public function name(): string; }
trait Greets { public function greet() {} }
final class User extends Model implements HasName {
    use Greets;
    private const MAX = 1;
    public function __construct(private string $name) {}
    public function name(): string { return $this->name; }
}
enum Status: string { case Active = 'a'; }
function helper($x) { return $x; }
