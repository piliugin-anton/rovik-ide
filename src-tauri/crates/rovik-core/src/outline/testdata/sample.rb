require 'json'
VERSION = "1.0"
module App
  class User < Base
    attr_reader :name
    def initialize(name)
      @name = name
    end
    def self.find(id)
    end
    class << self
      def build; end
    end
  end
end
def top_level; end
