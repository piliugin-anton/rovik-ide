package App::User;
use strict;
sub new {
    my ($class, %args) = @_;
    return bless {}, $class;
}
sub name { $_[0]{name} }
package main;
sub helper { 1 }
