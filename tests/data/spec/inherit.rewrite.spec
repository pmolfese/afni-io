
#define the group
	Group = subj

#define various States
	StateDef = smoothwm
	StateDef = inflated

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = FreeSurfer
	SurfaceName = ./lh.smoothwm.asc
	LocalDomainParent = SAME
	SurfaceState = smoothwm
	EmbedDimension = 3
	Anatomical = Y
	Hemisphere = L

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = FreeSurfer
	SurfaceName = ./lh.pial.asc
	LocalDomainParent = ./lh.smoothwm.asc
	SurfaceState = smoothwm
	EmbedDimension = 3
	Anatomical = Y
	LocalCurvatureParent = ./lh.smoothwm.asc

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = FreeSurfer
	SurfaceName = ./lh.inflated.asc
	LocalDomainParent = ./lh.smoothwm.asc
	SurfaceState = inflated
	EmbedDimension = 2
	LocalCurvatureParent = ./lh.smoothwm.asc
