defmodule App.User do
  @moduledoc "x"
  defstruct [:name]
  def new(name), do: %__MODULE__{name: name}
  defp helper(x) do
    x
  end
  defmacro m(x), do: x
end
defprotocol Size do
  def size(data)
end
