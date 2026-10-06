-module(app).
-export([start/0]).
-record(user, {name, age}).
-define(MAX, 10).
-type id() :: integer().
-spec start() -> ok.
start() ->
    ok.
helper(X) -> X;
helper(_) -> none.
