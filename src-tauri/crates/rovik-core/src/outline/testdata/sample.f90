module geometry
  implicit none
  type :: point
    real :: x
  end type point
contains
  function area(r) result(a)
    real :: r, a
    a = r
  end function area
  subroutine show(p)
    type(point) :: p
  end subroutine show
end module geometry
program main
  use geometry
end program main
