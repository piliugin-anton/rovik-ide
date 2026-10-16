import Foundation
let globalConstant = 1
protocol Shape { func area() -> Double; var name: String { get } }
struct Point: Shape {
    var x: Double
    init(x: Double) { self.x = x }
    func area() -> Double { return 0 }
}
class Node<T> {
    deinit {}
    subscript(i: Int) -> T? { nil }
}
enum Direction { case north, south }
extension Point { func moved() -> Point { self } }
typealias Distance = Double
func topLevel() {}
