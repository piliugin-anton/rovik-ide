module Main exposing (main)
import Html
type Msg = Increment | Decrement
type alias Model = { count : Int }
update : Msg -> Model -> Model
update msg model =
    model
main = Html.text "x"
