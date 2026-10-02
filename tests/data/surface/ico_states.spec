# define the group
	Group = QuickSpec
# define the various States
	StateDef = smoothwm
	StateDef = mirrored


NewSurface
	SurfaceType = FreeSurfer
	SurfaceState = smoothwm
	SurfaceName = ico.asc
	LocalDomainParent = SAME
	Anatomical = Y

NewSurface
	SurfaceType = FreeSurfer
	SurfaceState = mirrored
	SurfaceName = ico_mirror.asc
	LocalDomainParent = ico.asc
	Anatomical = N
