
#define the group
	Group = 

#define various States
	StateDef = smoothwm
	StateDef = pial

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = FreeSurfer
	SurfaceName = ./lh.smoothwm.asc
	LocalDomainParent = ./SAME
	SurfaceState = smoothwm
	EmbedDimension = 3
	LocalCurvatureParent = ./SAME

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = FreeSurfer
	SurfaceName = ./lh.pial.asc
	LocalDomainParent = ./lh.smoothwm.asc
	SurfaceState = pial
	EmbedDimension = 3
	LocalCurvatureParent = ./lh.smoothwm.asc
