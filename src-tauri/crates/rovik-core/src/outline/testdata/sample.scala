package app
import scala.util._
object Main extends App {
  val config = load()
  def run(args: Array[String]): Unit = { val local = 1 }
}
trait Shape { def area: Double }
case class Circle(r: Double) extends Shape { def area = 3.14 * r * r }
sealed trait Color
enum Planet { case Earth }
type Name = String
