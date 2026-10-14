package app
import kotlin.math.*
const val MAX = 10
data class Point(val x: Int, val y: Int)
interface Shape { fun area(): Double }
object Registry {
    val items = mutableListOf<String>()
    fun add(item: String) { val local = 1 }
}
class Circle(private val r: Double) : Shape {
    companion object { fun unit() = Circle(1.0) }
    override fun area(): Double = PI * r * r
    constructor() : this(1.0)
}
enum class Color { RED, GREEN }
typealias Names = List<String>
fun topLevel(x: Int): Int {
    val y = x
    return y
}
